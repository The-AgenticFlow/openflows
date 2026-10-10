use axum::response::Html;

/// Small browser console for exercising the local Manager connection flow.
pub async fn index() -> Html<&'static str> {
    Html(r###"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>OpenFlows Manager Console</title>
  <style>
    :root { color-scheme: dark; font-family: Inter, ui-sans-serif, system-ui, sans-serif; }
    body { margin: 0; background: #101318; color: #e9edf5; }
    main { max-width: 820px; margin: 48px auto; padding: 0 22px 60px; }
    h1 { font-size: 30px; margin-bottom: 8px; }
    p, label { color: #aeb8c9; line-height: 1.5; }
    section { background: #191e27; border: 1px solid #2d3543; border-radius: 12px; padding: 20px; margin: 18px 0; }
    button { background: #5b8cff; border: 0; border-radius: 7px; color: white; cursor: pointer; padding: 10px 14px; margin: 6px 6px 6px 0; font-weight: 600; }
    button.secondary { background: #303949; }
    button:disabled { opacity: .5; cursor: not-allowed; }
    input { display: block; width: min(100%, 420px); box-sizing: border-box; margin: 7px 0 12px; padding: 10px; border-radius: 7px; border: 1px solid #3b4658; background: #101318; color: #fff; }
    pre { white-space: pre-wrap; overflow-wrap: anywhere; background: #0c0f13; padding: 14px; border-radius: 8px; min-height: 50px; color: #c8d3e7; }
    .ok { color: #66e3a2; } .error { color: #ff8c8c; }
  </style>
</head>
<body>
<main>
  <h1>OpenFlows Manager Console</h1>
  <p>Small test UI for the browser sign-in and GitHub App connection flow.</p>

  <section>
    <h2>1. Sign in</h2>
    <p id="session">Checking browser session…</p>
    <button id="login">Sign in with GitHub</button>
    <button class="secondary" id="me">Refresh session</button>
  </section>

  <section>
    <h2>2. Organization</h2>
    <label>Slug <input id="slug" value="app-test-org"></label>
    <label>Display name <input id="display" value="App Test Organization"></label>
    <button id="create-org">Create organization</button>
    <pre id="org-output">No organization selected.</pre>
  </section>

  <section>
    <h2>3. GitHub App connection</h2>
    <button id="start" disabled>Start connection</button>
    <button class="secondary" id="status" disabled>Refresh connection status</button>
    <pre id="connection-output">Create an organization first.</pre>
  </section>

  <section>
    <h2>Activity</h2>
    <pre id="log"></pre>
  </section>
</main>
<script>
const $ = id => document.getElementById(id);
let csrfToken = null;
let organizationId = null;
let connection = null;

function log(message, error = false) {
  const line = new Date().toLocaleTimeString() + "  " + message;
  $('log').textContent = line + "\n" + $('log').textContent;
  $('log').className = error ? 'error' : '';
}
function show(id, value) { $(id).textContent = typeof value === 'string' ? value : JSON.stringify(value, null, 2); }
async function request(url, options = {}) {
  const response = await fetch(url, { credentials: 'include', ...options });
  const text = await response.text();
  let data; try { data = text ? JSON.parse(text) : {}; } catch { data = text; }
  if (!response.ok) throw new Error((data && data.error && data.error.message) || `${response.status} ${response.statusText}`);
  return data;
}
async function csrf() {
  const data = await request('/api/v1/auth/csrf');
  csrfToken = data.csrf_token;
  return csrfToken;
}
async function refreshSession() {
  try {
    const data = await request('/api/v1/me');
    show('session', `Signed in as ${data.display_name} (${data.id})`);
    $('session').className = 'ok';
    await csrf();
    log('Browser session is valid.');
  } catch (error) {
    show('session', 'Not signed in. Click “Sign in with GitHub”.');
    $('session').className = 'error';
    log(error.message, true);
  }
}
$('login').onclick = () => { window.location.href = '/auth/github/start?next=%2F'; };
$('me').onclick = refreshSession;
$('create-org').onclick = async () => {
  try {
    await csrf();
    const data = await request('/api/v1/organizations', { method: 'POST', headers: {
      'Content-Type': 'application/json', 'X-CSRF-Token': csrfToken, 'Idempotency-Key': `ui-org-${Date.now()}`
    }, body: JSON.stringify({ slug: $('slug').value.trim(), display_name: $('display').value.trim() }) });
    organizationId = data.resource_id;
    $('start').disabled = false; $('status').disabled = false;
    show('org-output', data); log(`Organization queued: ${organizationId}`);
  } catch (error) { log(error.message, true); show('org-output', error.message); }
};
$('start').onclick = async () => {
  try {
    await csrf();
    connection = await request(`/api/v1/organizations/${organizationId}/github/connect`, { method: 'POST', headers: {
      'Content-Type': 'application/json', 'X-CSRF-Token': csrfToken, 'Idempotency-Key': `ui-connect-${Date.now()}`
    }, body: JSON.stringify({ flow_type: 'both' }) });
    show('connection-output', connection); log('Connection flow created. Open both links.');
    if (connection.authorization_url) window.open(connection.authorization_url, '_blank', 'noopener');
    if (connection.setup_url) window.open(connection.setup_url, '_blank', 'noopener');
  } catch (error) { log(error.message, true); show('connection-output', error.message); }
};
$('status').onclick = async () => {
  try { show('connection-output', await request(`/api/v1/organizations/${organizationId}/github/connections`)); log('Connection status refreshed.'); }
  catch (error) { log(error.message, true); }
};
refreshSession();
</script>
</body>
</html>"###)
}
