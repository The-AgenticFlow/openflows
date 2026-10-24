//! Deployment commands. Setup owns generated state; doctor never starts workers.
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use fs2::FileExt;
use rand::RngCore;
use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use zeroize::Zeroizing;

const STATE: &str = "deployment.json";
// The Manager authenticates to Coder as the bootstrap administrator in managed
// installations. Keep its API key bounded, but renew it before expiry rather
// than inheriting Coder's seven-day administrator-key default.
const OPERATOR_TOKEN_LIFETIME_NANOS: i64 = 90 * 24 * 60 * 60 * 1_000_000_000;
const OPERATOR_ROTATION_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Serialize, Deserialize)]
struct Deployment {
    id: uuid::Uuid,
    coder_url: String,
    public_url: String,
    oauth_id: Option<String>,
    #[serde(default)]
    managed_admin_email: Option<String>,
    #[serde(default)]
    operator_user_id: Option<String>,
    #[serde(default)]
    operator_token_issued_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn setting(key: &str) -> Result<String> {
    std::env::var(key)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("configure {key} in your deployment file"))
}

fn default(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        std::env::set_var(key, value);
    }
}

/// Explicit files fail closed. Legacy deployments retain their existing defaults.
pub fn load_configuration(explicit: Option<&Path>) -> Result<()> {
    let selected = explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("OPENFLOWS_CONFIG_FILE").map(PathBuf::from))
        .or_else(|| {
            Path::new(".env.openflows")
                .exists()
                .then(|| PathBuf::from(".env.openflows"))
        });
    if let Some(path) = selected {
        dotenvy::from_path(&path)
            .map_err(|_| anyhow!("cannot parse deployment file {}", path.display()))?;
        let parent = path
            .canonicalize()?
            .parent()
            .context("configuration has no parent")?
            .to_path_buf();
        default("OPENFLOWS_MODE", "hosted");
        let hosted = std::env::var("OPENFLOWS_MODE").as_deref() == Ok("hosted");
        let root = if hosted {
            default("OPENFLOWS_SECRET_PROVIDER", "aws-secrets-manager");
            None
        } else {
            default("OPENFLOWS_SECRET_PROVIDER", "filesystem");
            let root = std::env::var_os("OPENFLOWS_SECRET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| parent.join(".openflows/secrets"));
            let root = if root.is_absolute() {
                root
            } else {
                parent.join(root)
            };
            std::env::set_var("OPENFLOWS_SECRET_DIR", &root);
            Some(root)
        };
        for (key, name) in [
            ("OPENFLOWS_CRYPTO_MASTER_KEY_REF", "master-key"),
            ("OPENFLOWS_CODER_HOOK_SECRET_REF", "coder-hook-secret"),
            ("OPENFLOWS_GITHUB_CLIENT_SECRET_REF", "github-client-secret"),
            ("OPENFLOWS_GITHUB_PRIVATE_KEY_REF", "github-app-private-key"),
            (
                "OPENFLOWS_GITHUB_WEBHOOK_SECRET_REF",
                "github-webhook-secret",
            ),
            ("OPENFLOWS_CODER_OPERATOR_TOKEN_REF", "coder-operator-token"),
            (
                "OPENFLOWS_CODER_OAUTH_CLIENT_SECRET_REF",
                "coder-oauth-client-secret",
            ),
        ] {
            default(key, name);
        }
        if let Some(root) = root {
            if let Some(state) = read_state(&root)? {
                state.validate()?;
                if let Some(id) = state.oauth_id {
                    default("OPENFLOWS_CODER_OAUTH_CLIENT_ID", &id);
                }
            }
        }
    } else if Path::new(".env.prod").exists() {
        dotenvy::from_path(".env.prod").map_err(|_| anyhow!("cannot parse legacy .env.prod"))?;
        eprintln!("Using legacy .env.prod; new deployments use --config .env.openflows.");
    }
    // The Docker provisioner runs Terraform on the Docker host. In local
    // development, make the Linux controller binary available to published
    // templates without requiring operators to copy an absolute path into
    // their environment. Production deployments use released workspace images
    // and never set this development-only variable.
    if std::env::var("OPENFLOWS_MODE").as_deref() == Ok("local")
        && std::env::var_os("TF_VAR_dev_binary_host_path").is_none()
    {
        if let Ok(path) = std::env::current_dir().map(|cwd| cwd.join(".dev-binaries")) {
            if path.is_dir() {
                if let Ok(path) = path.canonicalize() {
                    std::env::set_var("TF_VAR_dev_binary_host_path", path);
                }
            }
        }
    }
    Ok(())
}

impl Deployment {
    fn validate(&self) -> Result<()> {
        if self.coder_url != origin("CODER_URL")?
            || self.public_url != origin("OPENFLOWS_PUBLIC_URL")?
        {
            bail!("deployment URLs differ from saved setup state; restore the configured URLs or explicitly migrate this deployment");
        }
        if let (Some(saved), Ok(configured)) = (
            &self.oauth_id,
            std::env::var("OPENFLOWS_CODER_OAUTH_CLIENT_ID"),
        ) {
            if saved != &configured {
                bail!("configured Coder OAuth application differs from saved setup state; explicitly migrate the client and secret together");
            }
        }
        Ok(())
    }
}

fn origin(key: &str) -> Result<String> {
    let value = setting(key)?;
    let url = url::Url::parse(&value).map_err(|_| anyhow!("{key} must be an HTTP(S) origin"))?;
    let local = std::env::var("OPENFLOWS_MODE").as_deref() == Ok("local");
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || (local && loopback && url.scheme() == "http"))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("{key} must be an HTTPS origin (HTTP loopback is allowed in explicit local mode)");
    }
    Ok(value.trim_end_matches('/').into())
}

fn read_state(root: &Path) -> Result<Option<Deployment>> {
    match fs::read(root.join(STATE)) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).context("invalid deployment state")?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => bail!("cannot read deployment state"),
    }
}

fn save(root: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let mut file =
        tempfile::NamedTempFile::new_in(root).context("cannot stage deployment state")?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(root.join(name))
        .map_err(|_| anyhow!("cannot persist deployment state"))?;
    fs::File::open(root)?.sync_all()?;
    Ok(())
}

fn secret_path(root: &Path, key: &str) -> Result<PathBuf> {
    let name = setting(key)?;
    let path = PathBuf::from(&name);
    if path.is_absolute() {
        if !path.starts_with(root)
            || path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            bail!("{key} must stay inside OPENFLOWS_SECRET_DIR");
        }
        return Ok(path);
    }
    if path.components().count() != 1 || name == "." || name == ".." {
        bail!("{key} must be a secret name or an absolute file path");
    }
    Ok(root.join(path))
}

fn read_secret(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let bytes = fs::read(path)
        .map_err(|_| anyhow!("cannot read required secret file {}", path.display()))?;
    if bytes.is_empty() {
        bail!("secret file {} is empty", path.display());
    }
    Ok(Zeroizing::new(bytes))
}

fn random_key() -> Zeroizing<Vec<u8>> {
    let mut bytes = Zeroizing::new(vec![0; 32]);
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

fn random_hook_key() -> Zeroizing<String> {
    Zeroizing::new(base64::engine::general_purpose::STANDARD_NO_PAD.encode(&random_key()))
}

fn validate_hook_key(value: &[u8]) -> Result<()> {
    if value.len() < 32 || value.len() > 4096 || !value.iter().all(u8::is_ascii_graphic) {
        bail!("Coder hook secret must be 32-4096 printable ASCII bytes");
    }
    Ok(())
}

fn ensure_hook_key(path: &Path) -> Result<()> {
    match fs::metadata(path) {
        Ok(_) => validate_hook_key(&read_secret(path)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let value = random_hook_key();
            save(
                path.parent().context("hook secret has no parent")?,
                path.file_name()
                    .and_then(|s| s.to_str())
                    .context("invalid hook secret name")?,
                value.as_bytes(),
            )?;
            Ok(())
        }
        Err(_) => bail!("cannot inspect Coder hook secret"),
    }
}

fn ensure_key(path: &Path) -> Result<()> {
    // Never replace a key: existing ciphertext depends on it.
    match fs::metadata(path) {
        Ok(_) => {
            if read_secret(path)?.len() != 32 {
                bail!("master key must contain exactly 32 bytes");
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(path)?;
            file.write_all(&random_key())?;
            file.sync_all()?;
        }
        Err(_) => bail!("cannot inspect master key"),
    }
    Ok(())
}

struct Coder {
    base: String,
    http: Client,
}
impl Coder {
    fn new() -> Result<Self> {
        Ok(Self {
            base: origin("CODER_URL")?,
            http: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(20))
                .build()?,
        })
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> Result<reqwest::Response> {
        let mut request = self.http.request(method, format!("{}{path}", self.base));
        if !token.is_empty() {
            request = request.header("Coder-Session-Token", token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        request
            .send()
            .await
            .map_err(|_| anyhow!("Coder is unreachable at configured CODER_URL"))
    }
    async fn json(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let response = self.request(method, path, token, body).await?;
        if !response.status().is_success() {
            bail!("Coder {path} returned HTTP {}; check administrator permissions and OAuth provider availability", response.status());
        }
        response
            .json()
            .await
            .map_err(|_| anyhow!("Coder returned an invalid API response for {path}"))
    }

    async fn operator_identity(&self, token: &str) -> Result<Option<Value>> {
        let response = self
            .request(Method::GET, "/api/v2/users/me", token, None)
            .await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!(
                "Coder rejected the operator identity check with HTTP {}; refusing credential rotation",
                response.status()
            );
        }
        Ok(Some(
            response
                .json()
                .await
                .context("Coder returned an invalid operator identity")?,
        ))
    }
}

fn field(value: &Value, name: &str) -> Result<String> {
    value[name]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Coder response is missing {name}"))
}

async fn bootstrap(
    coder: &Coder,
    root: &Path,
    initialize: bool,
    email: Option<&str>,
) -> Result<Zeroizing<String>> {
    if initialize {
        let email = email.context("--initialize-coder requires --admin-email")?;
        let first = coder
            .request(Method::GET, "/api/v2/users/first", "", None)
            .await?;
        let password_path = root.join("coder-bootstrap-password");
        if first.status() == StatusCode::NOT_FOUND {
            if !password_path.exists() {
                save(
                    root,
                    "coder-bootstrap-password",
                    hex::encode(random_key()).as_bytes(),
                )?;
            }
            let password = read_secret(&password_path)?;
            let password = std::str::from_utf8(&password)?;
            coder
                .json(
                    Method::POST,
                    "/api/v2/users/first",
                    "",
                    Some(json!({
                        "email": email, "username":"openflows-admin", "password":password
                    })),
                )
                .await?;
        } else if !first.status().is_success() {
            bail!(
                "cannot inspect Coder initialization: HTTP {}",
                first.status()
            );
        }
        // On resume, only the saved bootstrap credential is used. No admin reset.
        let password = read_secret(&password_path)?;
        let login = coder
            .json(
                Method::POST,
                "/api/v2/users/login",
                "",
                Some(json!({
                    "email":email,"password":std::str::from_utf8(&password)?
                })),
            )
            .await?;
        return Ok(Zeroizing::new(field(&login, "session_token")?));
    }
    if let Ok(path) = std::env::var("OPENFLOWS_CODER_BOOTSTRAP_TOKEN_FILE") {
        return Ok(Zeroizing::new(
            String::from_utf8(read_secret(Path::new(&path))?.to_vec())?
                .trim()
                .into(),
        ));
    }
    let mut command = tokio::process::Command::new("coder");
    command
        .args(["--url", &coder.base, "login", "token"])
        .kill_on_drop(true);
    let result = tokio::time::timeout(Duration::from_secs(15), command.output()).await;
    if let Ok(Ok(output)) = result {
        if output.status.success() {
            let token = Zeroizing::new(String::from_utf8(output.stdout)?);
            if !token.trim().is_empty() {
                return Ok(Zeroizing::new(token.trim().into()));
            }
        }
    }
    bail!("authorize setup with 'coder login <CODER_URL>' or OPENFLOWS_CODER_BOOTSTRAP_TOKEN_FILE; for an empty managed deployment use --initialize-coder --admin-email <email>");
}

// The user link identifies the tenant creator. Chat creation is a privileged
// Manager operation with an explicit owner_id, not a delegated user OAuth call.
pub(crate) const CODER_USER_SCOPES: &str = "user:read_personal workspace:read";

/// Operator-only recovery for a hosted workspace whose single-use bootstrap
/// was consumed and whose process no longer has its rotating refresh token.
/// The new bootstrap is written only to the existing mounted secret bundle.
pub async fn repair_runtime(workspace_id: uuid::Uuid) -> Result<()> {
    use zeroize::Zeroize;

    let db = crate::db::Db::connect(&setting("OPENFLOWS_DATABASE_URL")?).await?;
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT role, runtime_secret_ref FROM workspaces
          WHERE id=$1 AND desired_state IN ('running','provisioning')
            AND observed_state NOT IN ('deleted','deleting')",
    )
    .bind(workspace_id)
    .fetch_optional(db.pool())
    .await?;
    let (role, bundle_ref) = row.context("recoverable workspace not found")?;
    if !matches!(
        role.as_str(),
        "nexus" | "forge" | "sentinel" | "vessel" | "lore"
    ) {
        bail!("runtime repair requires a hosted agent workspace");
    }
    let bundle_ref = bundle_ref.context("workspace has no mounted runtime secret")?;
    let provider_name = setting("OPENFLOWS_SECRET_PROVIDER")?;
    let provider: std::sync::Arc<dyn crate::secrets::SecretProvider> =
        crate::server::resolve_secret_provider(&provider_name)
            .await?
            .into();
    let repo = crate::runtime::RuntimeRepository::new(db.pool().clone());
    let credentials =
        crate::runtime::RuntimeCredentialService::with_secret_provider(repo, provider.clone());
    let mut bootstrap = credentials
        .issue_bootstrap(crate::id::WorkspaceId::from_uuid(workspace_id))
        .await?;
    let result = async {
        let bytes = Zeroizing::new(provider.get(&bundle_ref).await?);
        let mut payload: Value = serde_json::from_slice(&bytes)?;
        let object = payload
            .as_object_mut()
            .context("mounted runtime secret is not a JSON object")?;
        object.insert(
            "bootstrap_token".into(),
            Value::String(bootstrap.raw_secret.clone()),
        );
        object.insert(
            "manager_url".into(),
            Value::String(
                crate::config::runtime_manager_url(&setting("OPENFLOWS_PUBLIC_URL")?)
                    .map_err(anyhow::Error::msg)?,
            ),
        );
        let bytes = Zeroizing::new(serde_json::to_vec(&payload)?);
        provider.replace(&bundle_ref, &bytes).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    bootstrap.raw_secret.zeroize();
    result?;
    println!("Runtime bootstrap reissued for workspace {workspace_id}. Restart that Coder workspace to register a new session.");
    Ok(())
}

/// Reconcile the deployment's Coder hook verifier with the hook secret used
/// by Coder. The secret is accepted only on stdin and never echoed or logged.
pub async fn set_coder_hook_secret() -> Result<()> {
    use std::io::Read;
    let mut input = Zeroizing::new(Vec::new());
    std::io::stdin().take(4097).read_to_end(&mut input)?;
    while input
        .last()
        .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
    {
        input.pop();
    }
    validate_hook_key(&input)?;
    let provider_name = setting("OPENFLOWS_SECRET_PROVIDER")?;
    let provider = crate::server::resolve_secret_provider(&provider_name).await?;
    let secret_ref = setting("OPENFLOWS_CODER_HOOK_SECRET_REF")?;
    provider.replace(&secret_ref, &input).await?;
    println!("Coder lifecycle-hook secret updated. Restart Manager to load it.");
    Ok(())
}

async fn verify_coder_supported_scopes(coder: &Coder) -> Result<()> {
    let discovery = coder
        .json(
            Method::GET,
            "/.well-known/oauth-authorization-server",
            "",
            None,
        )
        .await?;
    let supported = discovery["scopes_supported"]
        .as_array()
        .context("Coder OAuth discovery does not list supported scopes")?;
    let missing: Vec<_> = CODER_USER_SCOPES
        .split_whitespace()
        .filter(|scope| !supported.iter().any(|value| value.as_str() == Some(*scope)))
        .collect();
    if !missing.is_empty() {
        bail!(
            "Coder at {} does not support required OAuth scopes: {}; upgrade Coder before linking accounts",
            coder.base,
            missing.join(", ")
        );
    }
    Ok(())
}

fn verify_app(app: &Value, callback: &str) -> Result<()> {
    let correct = app["redirect_uris"]
        .as_array()
        .is_some_and(|urls| urls.iter().any(|url| url == callback))
        || app["callback_url"] == callback;
    if !correct {
        bail!("existing Coder OAuth application's callback differs; explicitly update it before rerunning setup");
    }
    uuid::Uuid::parse_str(&field(app, "id")?).context("invalid Coder OAuth application ID")?;
    Ok(())
}

async fn ensure_identity_scopes(coder: &Coder, token: &str, app: Value) -> Result<Value> {
    let current: std::collections::HashSet<_> = app["scope"]
        .as_str()
        .unwrap_or("")
        .split_whitespace()
        .collect();
    if CODER_USER_SCOPES
        .split_whitespace()
        .all(|s| current.contains(s))
    {
        return Ok(app);
    }
    let id = field(&app, "id")?;
    let name = field(&app, "name")?;
    let mut uris: Vec<String> = app["redirect_uris"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|uri| uri.as_str().map(str::to_owned))
        .collect();
    if uris.is_empty() {
        uris.push(field(&app, "callback_url")?);
    }
    let icon = app["icon"].as_str().unwrap_or("");
    let mut scopes: Vec<&str> = current.into_iter().filter(|s| !s.is_empty()).collect();
    for required in CODER_USER_SCOPES.split_whitespace() {
        if !scopes.contains(&required) {
            scopes.push(required);
        }
    }
    scopes.sort_unstable();
    coder
        .json(
            Method::PUT,
            &format!("/api/v2/oauth2-provider/apps/{id}"),
            token,
            Some(json!({"name":name,"callback_url":uris[0],"redirect_uris":uris,"icon":icon,"scope":scopes.join(" ")})),
        )
        .await?;
    let updated = coder
        .json(
            Method::GET,
            &format!("/api/v2/oauth2-provider/apps/{id}"),
            token,
            None,
        )
        .await?;
    verify_identity_scopes(&updated)?;
    Ok(updated)
}

fn verify_identity_scopes(app: &Value) -> Result<()> {
    // Older Coder releases keep a blank application allowlist even after an
    // app update. A blank allowlist permits the client's requested scopes.
    if app["scope"].as_str().unwrap_or("").trim().is_empty() {
        return Ok(());
    }
    let current: std::collections::HashSet<_> = app["scope"]
        .as_str()
        .unwrap_or("")
        .split_whitespace()
        .collect();
    if CODER_USER_SCOPES
        .split_whitespace()
        .all(|scope| current.contains(scope))
    {
        Ok(())
    } else {
        bail!("Coder OAuth application lacks identity scopes; rerun Manager setup")
    }
}

pub async fn setup(initialize: bool, email: Option<&str>) -> Result<()> {
    if std::env::var("OPENFLOWS_MODE").as_deref() == Ok("hosted") {
        return setup_aws(initialize, email).await;
    }
    if setting("OPENFLOWS_SECRET_PROVIDER")? != "filesystem" {
        bail!("setup currently supports the durable filesystem secret provider");
    }
    let coder = Coder::new()?;
    verify_coder_supported_scopes(&coder).await?;
    let public_url = origin("OPENFLOWS_PUBLIC_URL")?;
    let root = PathBuf::from(setting("OPENFLOWS_SECRET_DIR")?);
    fs::create_dir_all(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("setup.lock"))?;
    lock.try_lock_exclusive()
        .context("another setup process is running")?;
    let mut state = read_state(&root)?.unwrap_or(Deployment {
        id: uuid::Uuid::new_v4(),
        coder_url: coder.base.clone(),
        public_url,
        oauth_id: std::env::var("OPENFLOWS_CODER_OAUTH_CLIENT_ID").ok(),
        managed_admin_email: None,
        operator_user_id: None,
        operator_token_issued_at: None,
    });
    state.validate()?;
    if initialize {
        let email = email.context("--initialize-coder requires --admin-email")?;
        if state
            .managed_admin_email
            .as_deref()
            .is_some_and(|saved| saved != email)
        {
            bail!("managed Coder bootstrap email differs from saved setup state");
        }
        state.managed_admin_email = Some(email.to_owned());
    }
    let configured_email = std::env::var("OPENFLOWS_CODER_BOOTSTRAP_EMAIL")
        .ok()
        .filter(|email| !email.trim().is_empty());
    if let (Some(saved), Some(configured)) = (&state.managed_admin_email, &configured_email) {
        if saved != configured {
            bail!("managed Coder bootstrap email differs from saved setup state");
        }
    }
    if state.managed_admin_email.is_none() {
        state.managed_admin_email = configured_email;
    }
    if state.managed_admin_email.is_some() && !root.join("coder-bootstrap-password").exists() {
        if secret_path(&root, "OPENFLOWS_CODER_OPERATOR_TOKEN_REF")?.exists() {
            bail!("this Coder installation has an operator token but no managed bootstrap password; remove OPENFLOWS_CODER_BOOTSTRAP_EMAIL and use your external credential provider");
        }
        let first = coder
            .request(Method::GET, "/api/v2/users/first", "", None)
            .await?;
        if first.status() != StatusCode::NOT_FOUND {
            bail!("Coder already has a first user or cannot be inspected; managed bootstrap requires a new Coder installation. Use an externally provisioned administrator credential instead");
        }
    }
    save(&root, STATE, &serde_json::to_vec(&state)?)?;
    ensure_key(&secret_path(&root, "OPENFLOWS_CRYPTO_MASTER_KEY_REF")?)?;
    ensure_hook_key(&secret_path(&root, "OPENFLOWS_CODER_HOOK_SECRET_REF")?)?;

    let operator_path = secret_path(&root, "OPENFLOWS_CODER_OPERATOR_TOKEN_REF")?;
    let existing_operator_token = operator_path
        .exists()
        .then(|| -> Result<Zeroizing<String>> {
            Ok(Zeroizing::new(
                String::from_utf8(read_secret(&operator_path)?.to_vec())?
                    .trim()
                    .into(),
            ))
        })
        .transpose()?;
    let existing_identity = if let Some(token) = &existing_operator_token {
        coder.operator_identity(token).await?
    } else {
        None
    };

    let rotate_due = state.managed_admin_email.is_some()
        && operator_path
            .metadata()
            .and_then(|metadata| metadata.modified())
            .and_then(|modified| modified.elapsed().map_err(std::io::Error::other))
            .is_ok_and(|age| age >= OPERATOR_ROTATION_AGE);

    // An expired key cannot create its own replacement. Re-authorize with the
    // explicitly supplied bootstrap credential, mint a bounded replacement,
    // and atomically overwrite only the operator-key secret. This makes setup
    // a recovery mechanism rather than leaving durable work permanently stuck.
    let token = if let (Some(token), Some(me)) = (existing_operator_token, existing_identity) {
        let user_id = field(&me, "id")?;
        if state
            .operator_user_id
            .as_deref()
            .is_some_and(|saved| saved != user_id)
        {
            bail!("Coder operator account differs from saved deployment identity");
        }
        state.operator_user_id.get_or_insert(user_id);
        if rotate_due {
            rotate_operator_token(&coder, &root, &operator_path, &mut state).await?
        } else {
            token
        }
    } else {
        if operator_path.exists() {
            eprintln!("Stored Coder operator key is invalid or expired; rotating it.");
        }
        rotate_operator_token(&coder, &root, &operator_path, &mut state).await?
    };
    save(&root, STATE, &serde_json::to_vec(&state)?)?;

    let callback = format!("{}/auth/coder/callback", state.public_url);
    let app = if let Some(id) = &state.oauth_id {
        uuid::Uuid::parse_str(id).context("invalid configured Coder OAuth application ID")?;
        coder
            .json(
                Method::GET,
                &format!("/api/v2/oauth2-provider/apps/{id}"),
                &token,
                None,
            )
            .await?
    } else {
        // Coder applies its resource-name validator to OAuth applications too.
        // Keep the deployment identity deterministic within its 32-character limit.
        let name = format!("of-{}", &state.id.simple().to_string()[..29]);
        let apps = coder
            .json(Method::GET, "/api/v2/oauth2-provider/apps", &token, None)
            .await?;
        let apps = apps
            .as_array()
            .context("invalid Coder OAuth application list")?;
        let matching: Vec<_> = apps.iter().filter(|app| app["name"] == name).collect();
        match matching.as_slice() {
            [app] => (*app).clone(),
            [] => coder.json(Method::POST, "/api/v2/oauth2-provider/apps", &token,
                // v2.37 requires callback_url; newer Coder accepts it alongside
                // the equivalent redirect_uris entry for compatibility.
                Some(json!({"name":name,"callback_url":callback,"redirect_uris":[callback],"scope":CODER_USER_SCOPES}))).await?,
            _ => bail!("multiple Coder OAuth applications match this deployment; resolve the duplicate before setup"),
        }
    };
    verify_app(&app, &callback)?;
    let app = ensure_identity_scopes(&coder, &token, app).await?;
    let id = field(&app, "id")?;
    state.oauth_id = Some(id.clone());
    save(&root, STATE, &serde_json::to_vec(&state)?)?;
    let oauth_path = secret_path(&root, "OPENFLOWS_CODER_OAUTH_CLIENT_SECRET_REF")?;
    if !oauth_path.exists() {
        let secret = coder
            .json(
                Method::POST,
                &format!("/api/v2/oauth2-provider/apps/{id}/secrets"),
                &token,
                None,
            )
            .await?;
        let secret = Zeroizing::new(field(&secret, "client_secret_full")?);
        save(
            oauth_path
                .parent()
                .context("OAuth secret needs a parent directory")?,
            oauth_path
                .file_name()
                .and_then(|s| s.to_str())
                .context("invalid OAuth secret path")?,
            secret.as_bytes(),
        )?;
    }
    read_secret(&oauth_path)?;
    println!("Setup complete. Internal key, Coder API token, and OAuth application are stored. Run doctor before serve.");
    Ok(())
}

async fn rotate_operator_token(
    coder: &Coder,
    root: &Path,
    operator_path: &Path,
    state: &mut Deployment,
) -> Result<Zeroizing<String>> {
    let bootstrap_token = bootstrap(
        coder,
        root,
        state.managed_admin_email.is_some(),
        state.managed_admin_email.as_deref(),
    )
    .await?;
    let me = coder
        .json(Method::GET, "/api/v2/users/me", &bootstrap_token, None)
        .await?;
    let user_id = field(&me, "id")?;
    uuid::Uuid::parse_str(&user_id)?;
    if state
        .operator_user_id
        .as_deref()
        .is_some_and(|saved| saved != user_id)
    {
        bail!("Coder bootstrap account differs from the saved operator identity");
    }
    let deployment = state.id.simple().to_string();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let minted = coder
        .json(
            Method::POST,
            &format!("/api/v2/users/{user_id}/keys/tokens"),
            &bootstrap_token,
            Some(json!({
                "token_name": format!("of-manager-{}-{}", &deployment[..8], &nonce[..8]),
                "lifetime": OPERATOR_TOKEN_LIFETIME_NANOS,
            })),
        )
        .await?;
    let key = Zeroizing::new(field(&minted, "key")?);
    save(
        operator_path
            .parent()
            .context("operator secret needs a parent directory")?,
        operator_path
            .file_name()
            .and_then(|s| s.to_str())
            .context("invalid operator secret path")?,
        key.as_bytes(),
    )?;
    state.operator_user_id = Some(user_id);
    state.operator_token_issued_at = Some(chrono::Utc::now());
    Ok(key)
}

async fn aws_read_optional(
    provider: &dyn crate::secrets::SecretProvider,
    name: &str,
) -> Result<Option<Vec<u8>>> {
    match provider.get(name).await {
        Ok(value) => Ok(Some(value)),
        Err(crate::secrets::SecretError::NotFound(_)) => Ok(None),
        Err(error) => Err(anyhow!(
            "cannot read managed deployment secret {name}: {error}"
        )),
    }
}

async fn aws_upsert(
    provider: &dyn crate::secrets::SecretProvider,
    name: &str,
    value: &[u8],
) -> Result<()> {
    if aws_read_optional(provider, name).await?.is_some() {
        provider.replace(name, value).await?;
    } else {
        provider.put(name, value).await?;
    }
    Ok(())
}

async fn aws_ensure_key(provider: &dyn crate::secrets::SecretProvider, name: &str) -> Result<()> {
    if let Some(existing) = aws_read_optional(provider, name).await? {
        if existing.len() != 32 {
            bail!("{name} must contain exactly 32 bytes");
        }
    } else {
        provider.put(name, &random_key()).await?;
    }
    Ok(())
}

async fn aws_ensure_hook_key(
    provider: &dyn crate::secrets::SecretProvider,
    name: &str,
) -> Result<()> {
    if let Some(existing) = aws_read_optional(provider, name).await? {
        validate_hook_key(&existing)?;
    } else {
        provider.put(name, random_hook_key().as_bytes()).await?;
    }
    Ok(())
}

async fn aws_bootstrap(
    coder: &Coder,
    provider: &dyn crate::secrets::SecretProvider,
    email: Option<&str>,
) -> Result<Zeroizing<String>> {
    if let Some(email) = email {
        let first = coder
            .request(Method::GET, "/api/v2/users/first", "", None)
            .await?;
        if first.status() == StatusCode::NOT_FOUND {
            if aws_read_optional(provider, "coder-bootstrap-password")
                .await?
                .is_none()
            {
                provider
                    .put(
                        "coder-bootstrap-password",
                        hex::encode(random_key()).as_bytes(),
                    )
                    .await?;
            }
            let password = Zeroizing::new(
                aws_read_optional(provider, "coder-bootstrap-password")
                    .await?
                    .context("missing Coder bootstrap password")?,
            );
            coder
                .json(
                    Method::POST,
                    "/api/v2/users/first",
                    "",
                    Some(json!({
                        "email": email,
                        "username": "openflows-admin",
                        "password": std::str::from_utf8(&password)?,
                    })),
                )
                .await?;
        } else if !first.status().is_success() {
            bail!(
                "cannot inspect Coder initialization: HTTP {}",
                first.status()
            );
        }
        let password = Zeroizing::new(
            aws_read_optional(provider, "coder-bootstrap-password")
                .await?
                .context("existing Coder cannot be claimed: no managed bootstrap password")?,
        );
        let login = coder
            .json(
                Method::POST,
                "/api/v2/users/login",
                "",
                Some(json!({"email": email, "password": std::str::from_utf8(&password)?})),
            )
            .await?;
        return Ok(Zeroizing::new(field(&login, "session_token")?));
    }
    let bootstrap_ref = setting("OPENFLOWS_CODER_BOOTSTRAP_TOKEN_REF")?;
    Ok(Zeroizing::new(
        String::from_utf8(provider.get(&bootstrap_ref).await?)?
            .trim()
            .to_owned(),
    ))
}

async fn setup_aws(initialize: bool, email: Option<&str>) -> Result<()> {
    if setting("OPENFLOWS_SECRET_PROVIDER")? != "aws-secrets-manager" {
        bail!("hosted setup requires OPENFLOWS_SECRET_PROVIDER=aws-secrets-manager");
    }
    // A database advisory lock serializes setup/renewal across Manager replicas.
    // A crashed process releases the lock when its connection closes.
    use sqlx::Connection;
    let mut lock = sqlx::PgConnection::connect(&setting("OPENFLOWS_DATABASE_URL")?)
        .await
        .context("cannot connect to the control-plane database for setup lock")?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(0x4f464c4f5753_i64)
        .execute(&mut lock)
        .await
        .context("cannot acquire hosted setup lock")?;

    let provider = crate::server::resolve_secret_provider("aws-secrets-manager").await?;
    let provider = provider.as_ref();
    let coder = Coder::new()?;
    verify_coder_supported_scopes(&coder).await?;
    let public_url = origin("OPENFLOWS_PUBLIC_URL")?;
    let mut state = match aws_read_optional(provider, STATE).await? {
        Some(bytes) => serde_json::from_slice::<Deployment>(&bytes)
            .context("invalid hosted deployment state")?,
        None => Deployment {
            id: uuid::Uuid::new_v4(),
            coder_url: coder.base.clone(),
            public_url,
            oauth_id: std::env::var("OPENFLOWS_CODER_OAUTH_CLIENT_ID").ok(),
            managed_admin_email: None,
            operator_user_id: None,
            operator_token_issued_at: None,
        },
    };
    state.validate()?;
    let configured_email = if initialize {
        Some(
            email
                .context("--initialize-coder requires --admin-email")?
                .to_owned(),
        )
    } else {
        std::env::var("OPENFLOWS_CODER_BOOTSTRAP_EMAIL")
            .ok()
            .filter(|email| !email.trim().is_empty())
    };
    if let (Some(saved), Some(configured)) = (&state.managed_admin_email, &configured_email) {
        if saved != configured {
            bail!("managed Coder bootstrap email differs from saved setup state");
        }
    }
    if state.managed_admin_email.is_none() {
        state.managed_admin_email = configured_email;
    }
    if state.managed_admin_email.is_some()
        && aws_read_optional(provider, "coder-bootstrap-password")
            .await?
            .is_none()
    {
        if aws_read_optional(provider, &setting("OPENFLOWS_CODER_OPERATOR_TOKEN_REF")?)
            .await?
            .is_some()
        {
            bail!("managed Coder has an operator token but no bootstrap password; do not adopt an existing installation implicitly");
        }
        let first = coder
            .request(Method::GET, "/api/v2/users/first", "", None)
            .await?;
        if first.status() != StatusCode::NOT_FOUND {
            bail!("managed bootstrap requires a new Coder installation with no first user");
        }
    }
    aws_upsert(provider, STATE, &serde_json::to_vec(&state)?).await?;
    aws_ensure_key(provider, &setting("OPENFLOWS_CRYPTO_MASTER_KEY_REF")?).await?;
    aws_ensure_hook_key(provider, &setting("OPENFLOWS_CODER_HOOK_SECRET_REF")?).await?;

    let operator_ref = setting("OPENFLOWS_CODER_OPERATOR_TOKEN_REF")?;
    let existing = aws_read_optional(provider, &operator_ref)
        .await?
        .map(|bytes| String::from_utf8(bytes).map(Zeroizing::new))
        .transpose()?;
    let identity = if let Some(token) = &existing {
        coder.operator_identity(token).await?
    } else {
        None
    };
    let rotate_due = state.managed_admin_email.is_some()
        && state.operator_token_issued_at.is_none_or(|issued| {
            chrono::Utc::now().signed_duration_since(issued)
                >= chrono::Duration::from_std(OPERATOR_ROTATION_AGE).unwrap()
        });
    let token = if let (Some(token), Some(me)) = (existing, identity) {
        let user_id = field(&me, "id")?;
        if state
            .operator_user_id
            .as_deref()
            .is_some_and(|saved| saved != user_id)
        {
            bail!("Coder operator account differs from saved deployment identity");
        }
        state.operator_user_id.get_or_insert(user_id);
        if rotate_due {
            rotate_operator_token_aws(&coder, provider, &operator_ref, &mut state).await?
        } else {
            token
        }
    } else {
        rotate_operator_token_aws(&coder, provider, &operator_ref, &mut state).await?
    };
    aws_upsert(provider, STATE, &serde_json::to_vec(&state)?).await?;

    let callback = format!("{}/auth/coder/callback", state.public_url);
    let app = if let Some(id) = &state.oauth_id {
        uuid::Uuid::parse_str(id).context("invalid configured Coder OAuth application ID")?;
        coder
            .json(
                Method::GET,
                &format!("/api/v2/oauth2-provider/apps/{id}"),
                &token,
                None,
            )
            .await?
    } else {
        let name = format!("of-{}", &state.id.simple().to_string()[..29]);
        let apps = coder
            .json(Method::GET, "/api/v2/oauth2-provider/apps", &token, None)
            .await?;
        let apps = apps
            .as_array()
            .context("invalid Coder OAuth application list")?;
        let matching: Vec<_> = apps.iter().filter(|app| app["name"] == name).collect();
        match matching.as_slice() {
            [app] => (*app).clone(),
            [] => coder
                .json(
                    Method::POST,
                    "/api/v2/oauth2-provider/apps",
                    &token,
                    Some(json!({"name": name, "callback_url": callback, "redirect_uris": [callback], "scope": CODER_USER_SCOPES})),
                )
                .await?,
            _ => bail!("multiple Coder OAuth applications match this deployment"),
        }
    };
    verify_app(&app, &callback)?;
    let app = ensure_identity_scopes(&coder, &token, app).await?;
    let id = field(&app, "id")?;
    state.oauth_id = Some(id.clone());
    aws_upsert(provider, STATE, &serde_json::to_vec(&state)?).await?;
    let oauth_ref = setting("OPENFLOWS_CODER_OAUTH_CLIENT_SECRET_REF")?;
    if aws_read_optional(provider, &oauth_ref).await?.is_none() {
        let secret = coder
            .json(
                Method::POST,
                &format!("/api/v2/oauth2-provider/apps/{id}/secrets"),
                &token,
                None,
            )
            .await?;
        let secret = Zeroizing::new(field(&secret, "client_secret_full")?);
        provider.put(&oauth_ref, secret.as_bytes()).await?;
    }
    println!("Hosted setup complete. Platform secrets are stored in AWS Secrets Manager.");
    Ok(())
}

async fn rotate_operator_token_aws(
    coder: &Coder,
    provider: &dyn crate::secrets::SecretProvider,
    operator_ref: &str,
    state: &mut Deployment,
) -> Result<Zeroizing<String>> {
    let bootstrap = aws_bootstrap(coder, provider, state.managed_admin_email.as_deref()).await?;
    let me = coder
        .json(Method::GET, "/api/v2/users/me", &bootstrap, None)
        .await?;
    let user_id = field(&me, "id")?;
    uuid::Uuid::parse_str(&user_id)?;
    if state
        .operator_user_id
        .as_deref()
        .is_some_and(|saved| saved != user_id)
    {
        bail!("Coder bootstrap account differs from saved operator identity");
    }
    let deployment = state.id.simple().to_string();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let minted = coder
        .json(
            Method::POST,
            &format!("/api/v2/users/{user_id}/keys/tokens"),
            &bootstrap,
            Some(json!({
                "token_name": format!("of-manager-{}-{}", &deployment[..8], &nonce[..8]),
                "lifetime": OPERATOR_TOKEN_LIFETIME_NANOS,
            })),
        )
        .await?;
    let key = Zeroizing::new(field(&minted, "key")?);
    aws_upsert(provider, operator_ref, key.as_bytes()).await?;
    state.operator_user_id = Some(user_id);
    state.operator_token_issued_at = Some(chrono::Utc::now());
    Ok(key)
}

/// A managed Coder installation retains its one-time bootstrap password so
/// setup can renew Manager authority without an interactive Coder session.
pub async fn managed_bootstrap_email() -> Result<Option<String>> {
    if std::env::var("OPENFLOWS_MODE").as_deref() == Ok("hosted") {
        if setting("OPENFLOWS_SECRET_PROVIDER")? != "aws-secrets-manager" {
            bail!("hosted mode requires OPENFLOWS_SECRET_PROVIDER=aws-secrets-manager");
        }
        let provider = crate::server::resolve_secret_provider("aws-secrets-manager").await?;
        let Some(state) = aws_read_optional(provider.as_ref(), STATE).await? else {
            return Ok(None);
        };
        let state: Deployment = serde_json::from_slice(&state)?;
        return if aws_read_optional(provider.as_ref(), "coder-bootstrap-password")
            .await?
            .is_some()
        {
            Ok(state.managed_admin_email)
        } else {
            Ok(None)
        };
    }
    let root = match std::env::var("OPENFLOWS_SECRET_DIR") {
        Ok(root) => PathBuf::from(root),
        Err(_) => return Ok(None),
    };
    let Some(state) = read_state(&root)? else {
        return Ok(None);
    };
    if root.join("coder-bootstrap-password").is_file() {
        Ok(state.managed_admin_email)
    } else {
        Ok(None)
    }
}

/// Restore the generated Coder OAuth application ID from durable hosted state
/// before typed configuration is parsed. No secret value is exported to env.
pub async fn load_hosted_configuration() -> Result<()> {
    if std::env::var("OPENFLOWS_MODE").as_deref() != Ok("hosted") {
        return Ok(());
    }
    if setting("OPENFLOWS_SECRET_PROVIDER")? != "aws-secrets-manager" {
        bail!("hosted mode requires OPENFLOWS_SECRET_PROVIDER=aws-secrets-manager");
    }
    let provider = crate::server::resolve_secret_provider("aws-secrets-manager").await?;
    if let Some(bytes) = aws_read_optional(provider.as_ref(), STATE).await? {
        let state: Deployment = serde_json::from_slice(&bytes)?;
        state.validate()?;
        if let Some(id) = state.oauth_id {
            default("OPENFLOWS_CODER_OAUTH_CLIENT_ID", &id);
        }
    }
    Ok(())
}

/// Run every diagnostic even after failures; never migrate or provision.
pub async fn doctor() -> Result<()> {
    let mut failed = false;
    let mut report = |label: &str, result: Result<()>| match result {
        Ok(()) => println!("PASS {label}"),
        Err(error) => {
            failed = true;
            println!("FAIL {label}: {error}");
        }
    };
    if std::env::var("OPENFLOWS_MODE").as_deref() == Ok("hosted") {
        report("hosted deployment state", load_hosted_configuration().await);
    }
    report(
        "typed configuration",
        crate::config::ManagerConfig::from_env()
            .map(|_| ())
            .map_err(anyhow::Error::msg),
    );
    report("public origin", origin("OPENFLOWS_PUBLIC_URL").map(|_| ()));
    report(
        "runtime Manager origin",
        setting("OPENFLOWS_PUBLIC_URL")
            .and_then(|public| {
                crate::config::runtime_manager_url(&public).map_err(anyhow::Error::msg)
            })
            .map(|_| ()),
    );
    report(
        "Coder lifecycle hook origin",
        setting("OPENFLOWS_PUBLIC_URL")
            .and_then(|public| crate::config::coder_hook_url(&public).map_err(anyhow::Error::msg))
            .map(|_| ()),
    );
    report(
        "runtime hook routing",
        crate::config::runtime_hook_url(uuid::Uuid::nil())
            .map(|_| ())
            .map_err(anyhow::Error::msg),
    );
    // Resolve the secret directory as a reported check so a missing directory
    // does not abort the remaining independent diagnostics (PostgreSQL, Redis,
    // Coder, Docker). Secret-dependent checks below report FAIL on their own.
    let hosted = std::env::var("OPENFLOWS_MODE").as_deref() == Ok("hosted");
    let root = if hosted {
        PathBuf::new()
    } else {
        match setting("OPENFLOWS_SECRET_DIR") {
            Ok(dir) => PathBuf::from(dir),
            Err(error) => {
                report("secret directory", Err(error));
                PathBuf::new()
            }
        }
    };
    let provider = if hosted {
        match crate::server::resolve_secret_provider("aws-secrets-manager").await {
            Ok(provider) => Some(provider),
            Err(error) => {
                report("AWS Secrets Manager", Err(anyhow!(error)));
                None
            }
        }
    } else {
        None
    };
    for key in [
        "OPENFLOWS_CRYPTO_MASTER_KEY_REF",
        "OPENFLOWS_CODER_HOOK_SECRET_REF",
        "OPENFLOWS_GITHUB_CLIENT_SECRET_REF",
        "OPENFLOWS_GITHUB_PRIVATE_KEY_REF",
        "OPENFLOWS_GITHUB_WEBHOOK_SECRET_REF",
        "OPENFLOWS_CODER_OPERATOR_TOKEN_REF",
        "OPENFLOWS_CODER_OAUTH_CLIENT_SECRET_REF",
    ] {
        report(
            key,
            async {
                let secret = if let Some(provider) = provider.as_ref() {
                    Zeroizing::new(provider.get(&setting(key)?).await?)
                } else {
                    read_secret(&secret_path(&root, key)?)?
                };
                if key == "OPENFLOWS_CRYPTO_MASTER_KEY_REF" && secret.len() != 32 {
                    bail!("expected 32 bytes");
                }
                if key == "OPENFLOWS_GITHUB_PRIVATE_KEY_REF" {
                    jsonwebtoken::EncodingKey::from_rsa_pem(&secret)
                        .map_err(|_| anyhow!("invalid RSA private key"))?;
                }
                Ok(())
            }
            .await,
        );
    }
    report(
        "PostgreSQL",
        async {
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_secs(5))
                .connect(&setting("OPENFLOWS_DATABASE_URL")?)
                .await
                .map_err(|_| anyhow!("cannot connect to the control-plane database"))?;
            let ping = sqlx::query("SELECT 1").execute(&pool).await;
            pool.close().await;
            ping.map_err(|_| anyhow!("database query failed"))?;
            Ok(())
        }
        .await,
    );
    report(
        "Redis",
        async {
            use fred::prelude::*;
            let url = setting("OPENFLOWS_REDIS_ADMIN_URL")?;
            let config = fred::types::config::Config::from_url(&url)
                .map_err(|_| anyhow!("invalid Redis URL"))?;
            let client = fred::types::Builder::from_config(config)
                .build()
                .map_err(|_| anyhow!("invalid Redis configuration"))?;
            let result = tokio::time::timeout(Duration::from_secs(5), client.init()).await;
            let _ = tokio::time::timeout(Duration::from_secs(2), client.quit()).await;
            result
                .map_err(|_| anyhow!("Redis connection timed out"))?
                .map_err(|_| anyhow!("Redis connection failed"))?;
            Ok(())
        }
        .await,
    );
    report(
        "Coder identity and OAuth registration",
        async {
            let coder = Coder::new()?;
            verify_coder_supported_scopes(&coder).await?;
            let token = if let Some(provider) = provider.as_ref() {
                Zeroizing::new(
                    provider
                        .get(&setting("OPENFLOWS_CODER_OPERATOR_TOKEN_REF")?)
                        .await?,
                )
            } else {
                read_secret(&secret_path(&root, "OPENFLOWS_CODER_OPERATOR_TOKEN_REF")?)?
            };
            let token = std::str::from_utf8(&token)?.trim();
            coder
                .json(Method::GET, "/api/v2/users/me", token, None)
                .await?;
            let id = setting("OPENFLOWS_CODER_OAUTH_CLIENT_ID")?;
            uuid::Uuid::parse_str(&id).context("invalid OAuth application ID")?;
            let app = coder
                .json(
                    Method::GET,
                    &format!("/api/v2/oauth2-provider/apps/{id}"),
                    token,
                    None,
                )
                .await?;
            verify_app(
                &app,
                &format!("{}/auth/coder/callback", origin("OPENFLOWS_PUBLIC_URL")?),
            )?;
            verify_identity_scopes(&app)
        }
        .await,
    );
    report(
        "template source configuration",
        crate::config::ReleaseSourceConfig::from_env()
            .map(|_| ())
            .map_err(|_| anyhow!("invalid template release configuration")),
    );
    if std::env::var("OPENFLOWS_PROVISIONER_BACKEND").unwrap_or_else(|_| "docker".into())
        == "docker"
    {
        report(
            "Docker provisioner host",
            async {
                let binary =
                    std::env::var("OPENFLOWS_DOCKER_BINARY").unwrap_or_else(|_| "docker".into());
                let output = tokio::time::timeout(
                    Duration::from_secs(10),
                    tokio::process::Command::new(binary)
                        .args(["info", "--format", "{{.ServerVersion}}"])
                        .kill_on_drop(true)
                        .output(),
                )
                .await
                .map_err(|_| anyhow!("Docker daemon timed out"))?
                .map_err(|_| anyhow!("Docker executable is unavailable"))?;
                if !output.status.success() {
                    bail!("Docker daemon is unavailable");
                }
                Ok(())
            }
            .await,
        );
    }
    if failed {
        bail!("deployment checks failed; no resources were provisioned");
    }
    println!("Checks passed. Browser authorization, workspace-to-Coder connectivity, and provisioning still require an end-to-end test.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_preserves_master_key_and_rejects_invalid_existing_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("master");
        ensure_key(&path).unwrap();
        let original = fs::read(&path).unwrap();
        ensure_key(&path).unwrap();
        assert_eq!(original, fs::read(&path).unwrap());
        fs::write(&path, b"invalid").unwrap();
        assert!(ensure_key(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"invalid");
    }
    #[test]
    fn setup_preserves_printable_hook_key_and_rejects_binary_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hook");
        ensure_hook_key(&path).unwrap();
        let original = fs::read(&path).unwrap();
        assert!(original.len() >= 32);
        assert!(original.iter().all(u8::is_ascii_graphic));
        ensure_hook_key(&path).unwrap();
        assert_eq!(original, fs::read(&path).unwrap());
        fs::write(&path, [0; 32]).unwrap();
        assert!(ensure_hook_key(&path).is_err());
    }
    #[test]
    fn oauth_reuse_requires_exact_callback_and_valid_id() {
        let app = json!({"id":uuid::Uuid::new_v4(),"redirect_uris":["https://flows.test/auth/coder/callback"]});
        assert!(verify_app(&app, "https://flows.test/auth/coder/callback").is_ok());
        assert!(verify_app(&app, "https://other.test/auth/coder/callback").is_err());
    }
}
