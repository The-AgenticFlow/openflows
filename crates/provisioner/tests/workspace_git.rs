//! Provision real instructions into a Git checkout without hiding candidate changes.
use anyhow::Result;
use async_trait::async_trait;
use provisioner::{CommandOutput, DirEntry, Provisioner, WorkspaceTransport};
use std::path::{Path, PathBuf};
use std::process::Command;

struct LocalWorkspace(PathBuf);

#[async_trait]
impl WorkspaceTransport for LocalWorkspace {
    async fn read_file(&self, path: &str) -> Result<String> {
        Ok(std::fs::read_to_string(self.0.join(path))?)
    }
    async fn write_file(&self, path: &str, content: &str) -> Result<()> {
        let target = self.0.join(path);
        std::fs::create_dir_all(target.parent().unwrap())?;
        Ok(std::fs::write(target, content)?)
    }
    async fn execute(&self, command: &str) -> Result<CommandOutput> {
        let output = Command::new("bash")
            .args(["-c", command])
            .current_dir(&self.0)
            .output()?;
        Ok(CommandOutput {
            stdout: String::from_utf8(output.stdout)?,
            stderr: String::from_utf8(output.stderr)?,
            exit_code: output.status.code().unwrap_or(-1),
        })
    }
    async fn copy_file(&self, source: &Path, target: &str) -> Result<()> {
        self.write_file(target, &std::fs::read_to_string(source)?)
            .await
    }
    async fn symlink_or_copy(&self, source: &Path, target: &str) -> Result<()> {
        self.copy_file(source, target).await
    }
    async fn create_dir_all(&self, path: &str) -> Result<()> {
        Ok(std::fs::create_dir_all(self.0.join(path))?)
    }
    async fn path_exists(&self, path: &str) -> bool {
        self.0.join(path).exists()
    }
    async fn remove_dir_all(&self, path: &str) -> Result<()> {
        Ok(std::fs::remove_dir_all(self.0.join(path))?)
    }
    async fn list_directory(&self, _path: &str) -> Result<Vec<DirEntry>> {
        anyhow::bail!("Directory listing is not needed by provisioning")
    }
}

#[tokio::test]
async fn provisioning_keeps_git_clean_but_preserves_visibility_of_candidate_changes() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let workspace = LocalWorkspace(directory.path().to_owned());
    let setup = workspace.execute("set -eu; git init -q; git config user.name Test; git config user.email test@example.test; git config commit.gpgsign false; printf '41\n' > answer.txt; git add answer.txt; git commit -qm initial; printf '/existing-local-file\n' >> .git/info/exclude").await?;
    assert_eq!(setup.exit_code, 0, "{}", setup.stderr);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let registry = config::Registry::load(root.join("orchestration/agent/registry.json"))?;
    let provisioner = Provisioner::new(root);
    provisioner
        .provision_role(&workspace, "forge", &registry)
        .await?;
    assert!(workspace.read_file("AGENTS.md").await?.contains("FORGE"));
    let status = workspace.execute("git status --porcelain").await?;
    assert_eq!(status.exit_code, 0);
    assert_eq!(
        status.stdout, "",
        "Provisioning must not dirty the candidate"
    );
    let excludes = workspace.read_file(".git/info/exclude").await?;
    assert!(excludes.contains("/existing-local-file"));
    provisioner
        .provision_role(&workspace, "forge", &registry)
        .await?;
    assert_eq!(workspace.read_file(".git/info/exclude").await?, excludes);
    workspace.write_file("answer.txt", "42\n").await?;
    workspace
        .write_file("new-source.txt", "candidate work\n")
        .await?;
    let status = workspace.execute("git status --porcelain").await?;
    assert!(status.stdout.contains(" M answer.txt"));
    assert!(status.stdout.contains("?? new-source.txt"));
    let track = workspace
        .execute("git add -f AGENTS.md; git commit -qm track-instructions")
        .await?;
    assert_eq!(track.exit_code, 0, "{}", track.stderr);
    workspace
        .write_file("AGENTS.md", "Changed tracked instructions\n")
        .await?;
    assert!(workspace
        .execute("git status --porcelain")
        .await?
        .stdout
        .contains(" M AGENTS.md"));
    Ok(())
}
