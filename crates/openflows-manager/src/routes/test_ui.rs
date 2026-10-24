use axum::response::Html;

/// Small browser console for exercising the local Manager connection flow.
pub async fn index() -> Html<&'static str> {
    Html(
        r###"<!doctype html>
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
    button.danger { background: #b53a4a; }
    button:disabled { opacity: .5; cursor: not-allowed; }
    input { display: block; width: min(100%, 420px); box-sizing: border-box; margin: 7px 0 12px; padding: 10px; border-radius: 7px; border: 1px solid #3b4658; background: #101318; color: #fff; }
    pre { white-space: pre-wrap; overflow-wrap: anywhere; background: #0c0f13; padding: 14px; border-radius: 8px; min-height: 50px; color: #c8d3e7; }
    .ok { color: #66e3a2; } .error { color: #ff8c8c; }
    select { display: block; width: 100%; padding: 10px; margin: 8px 0; background: #101318; color: #fff; border: 1px solid #3b4658; border-radius: 7px; }
  </style>
</head>
<body>
<main>
  <h1>OpenFlows Manager Console</h1>
  <p>Test sign-in, GitHub connections, and tenant provisioning.</p>

  <section>
    <h2>1. Sign in</h2>
    <p id="session">Checking browser session…</p>
    <button id="login">Sign in with GitHub</button>
    <button class="secondary" id="me">Refresh session</button>
    <button class="secondary" id="link-coder">Connect my GitHub-backed Coder account</button>
    <button class="secondary" id="coder-status">Check Coder account</button>
    <pre id="coder-account"></pre>
    <p>Connect the personal Coder account that should own your tenants. Existing links cannot be silently replaced.</p>
  </section>

  <section>
    <h2>2. Organization</h2>
    <label>Existing organization <select id="organizations"><option value="">Select an organization</option></select></label>
    <button class="secondary" id="refresh-orgs">Refresh organizations</button>
    <label>Slug <input id="slug" value="app-test-org"></label>
    <label>Display name <input id="display" value="App Test Organization"></label>
    <button id="create-org">Create organization</button>
    <button class="secondary" id="promote-release" disabled>Promote configured template release</button>
    <p>Platform operators can publish the Manager’s configured template release to this organization.</p>
    <pre id="org-output">No organization selected.</pre>
  </section>

  <section>
    <h2>3. GitHub App connection</h2>
    <button id="start" disabled>Start connection</button>
    <button class="secondary" id="status" disabled>Refresh connection status</button>
    <pre id="connection-output">Create an organization first.</pre>
  </section>

  <section>
    <h2>4. Tenants</h2>
    <p id="tenant-help">Select an organization. It must be ready and the GitHub App must have access to the repository before creating a tenant.</p>
    <label>GitHub repository URL <input id="repository-url" type="url" placeholder="https://github.com/OWNER/REPO" autocomplete="url"></label>
    <p>Paste the repository URL. It must be accessible to this organization’s active GitHub App installation.</p>
    <label>Tenant name <input id="tenant-name" placeholder="my-repo"></label>
    <label>Fleet size <input id="fleet" type="number" min="1" step="1" value="1"></label>
    <button id="create-tenant" disabled>Create tenant</button>
    <label>Existing tenant <select id="tenants" disabled><option value="">Select a tenant</option></select></label>
    <button class="secondary" id="refresh-tenants" disabled>Refresh tenants</button>
    <button id="start-tenant" disabled>Start tenant</button>
    <button class="secondary" id="stop-tenant" disabled>Stop tenant</button>
    <button class="danger" id="delete-tenant" disabled>Delete tenant</button>
    <button class="secondary" id="tenant-status" disabled>Refresh tenant status</button>
    <p>Deleting a tenant revokes its runtime credentials, removes its Coder workspaces, and releases its provisioning capacity. This cannot be undone.</p>
    <pre id="tenant-output" aria-live="polite">No tenant selected.</pre>
    <h3>Operation progress</h3>
    <label>Operation ID <input id="operation-id" placeholder="Paste an operation ID to inspect or retry"></label>
    <button class="secondary" id="inspect-operation">Refresh operation</button>
    <button class="secondary" id="retry-operation" disabled>Retry operation</button>
    <p>Progress refreshes every three seconds until the operation finishes. Errors include the failed step when available.</p>
    <pre id="operation-output" aria-live="polite">No operation selected.</pre>
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
let selectedTenant = null;
let tenantId = null;
let operationTimer = null;
let operationGeneration = 0;
let busy = false;
let signedIn = false;
let selectedOrg = null;

function controls() {
  for (const id of ['create-org', 'refresh-orgs']) $(id).disabled = busy || !signedIn;
  $('promote-release').disabled = busy || !signedIn || !organizationId || selectedOrg?.status !== 'ready';
  $('me').disabled = busy;
  $('organizations').disabled = busy || !signedIn;
  for (const id of ['start', 'status', 'refresh-tenants', 'tenants']) $(id).disabled = busy || !organizationId;
  $('create-tenant').disabled = busy || !organizationId || selectedOrg?.status !== 'ready';
  $('start-tenant').disabled = busy || !tenantId || selectedTenant?.observed_state === 'provisioning';
  $('stop-tenant').disabled = busy || !tenantId;
  $('delete-tenant').disabled = busy || !tenantId || ['deleting', 'deleted'].includes(selectedTenant?.desired_state);
  $('tenant-status').disabled = busy || !tenantId;
}
function cancelOperation() { clearTimeout(operationTimer); operationGeneration++; }
async function action(work, output) {
  if (busy) return;
  busy = true; controls();
  try { await work(); } catch (error) { log(error.message, true); if (output) show(output, error.message); }
  finally { busy = false; controls(); }
}
async function mutate(url, body = {}) {
  await csrf();
  return request(url, { method: 'POST', headers: {
    'Content-Type': 'application/json', 'X-CSRF-Token': csrfToken, 'Idempotency-Key': crypto.randomUUID()
  }, body: JSON.stringify(body) });
}
async function destroy(url) {
  await csrf();
  return request(url, { method: 'DELETE', headers: {
    'X-CSRF-Token': csrfToken, 'Idempotency-Key': crypto.randomUUID()
  } });
}
async function pages(url) {
  const items = []; let after = null;
  do {
    const page = await request(url + '?limit=100' + (after ? '&after=' + encodeURIComponent(after) : ''));
    items.push(...page.items); after = page.next_cursor;
  } while (after);
  return items;
}
function option(select, value, label) {
  const item = document.createElement('option'); item.value = value; item.textContent = label; select.append(item);
}
async function refreshOrganizations(preferred = organizationId) {
  const orgs = await pages('/api/v1/organizations');
  $('organizations').replaceChildren(); option($('organizations'), '', 'Select an organization');
  for (const org of orgs) option($('organizations'), org.organization_id, `${org.display_name} — ${org.status}`);
  selectedOrg = orgs.find(org => org.organization_id === preferred) || null;
  organizationId = selectedOrg?.organization_id || null;
  if (!organizationId) { tenantId = null; selectedTenant = null; $('tenants').replaceChildren(); option($('tenants'), '', 'Select a tenant'); }
  $('organizations').value = organizationId || '';
  show('org-output', selectedOrg || 'Select or create an organization.');
  controls();
}
async function refreshTenants(preferred = tenantId) {
  const org = organizationId;
  const tenants = await pages(`/api/v1/organizations/${org}/tenants`);
  if (org !== organizationId) return;
  $('tenants').replaceChildren(); option($('tenants'), '', 'Select a tenant');
  for (const tenant of tenants) option($('tenants'), tenant.id, `${tenant.slug} — ${tenant.observed_state}`);
  selectedTenant = tenants.find(tenant => tenant.id === preferred) || null;
  tenantId = selectedTenant?.id || null;
  $('tenants').value = tenantId || ''; controls();
}
async function tenantStatus() {
  const org = organizationId, tenant = tenantId;
  const base = `/api/v1/organizations/${org}/tenants/${tenant}`;
  const [details, runtime] = await Promise.all([request(base), request(base + '/status')]);
  if (org === organizationId && tenant === tenantId) selectedTenant = details;
  if (org === organizationId && tenant === tenantId) show('tenant-output', { ...details, runtime });
  return runtime;
}
function watchOperation(id) {
  cancelOperation();
    $('operation-id').value = id;
    $('retry-operation').disabled = true;
  const generation = operationGeneration, org = organizationId;
  const poll = async () => {
    try {
      const operation = await request(`/api/v1/operations/${encodeURIComponent(id)}`);
      if (generation !== operationGeneration || org !== organizationId) return;
      show('operation-output', operation);
      $('retry-operation').disabled = !(operation.state === 'retryable_failed' ||
        (operation.state === 'failed' && (operation.kind === 'template.promote' ||
          (operation.kind === 'org.provision' && operation.error_code === 'IMPORT_FAILED'))));
      if (['succeeded', 'failed', 'cancelled'].includes(operation.state)) {
        log(`Operation ${operation.state}${operation.error_code ? ': ' + operation.error_code : ''}`, operation.state !== 'succeeded');
        await refreshOrganizations(org);
        if (organizationId === org) { await refreshTenants(); if (tenantId) await tenantStatus(); }
        return;
      }
      operationTimer = setTimeout(poll, 3000);
    } catch (error) { if (generation === operationGeneration) { show('operation-output', error.message); log(error.message, true); } }
  };
  poll();
}

function log(message, error = false) {
  const line = new Date().toLocaleTimeString() + "  " + message;
  $('log').textContent = line + "\n" + $('log').textContent;
  $('log').className = error ? 'error' : '';
}
function show(id, value) { $(id).textContent = typeof value === 'string' ? value : JSON.stringify(value, null, 2); }
function showConnection(value) {
  const output = $('connection-output');
  output.textContent = '';
  if (value.authorization_url) {
    const link = document.createElement('a'); link.href = value.authorization_url;
    link.target = '_blank'; link.rel = 'noopener'; link.textContent = 'Open GitHub authorization';
    output.append(link, document.createTextNode('\n'));
  }
  if (value.setup_url) {
    const link = document.createElement('a'); link.href = value.setup_url;
    link.target = '_blank'; link.rel = 'noopener'; link.textContent = 'Open GitHub App installation';
    output.append(link, document.createTextNode('\n'));
  }
  const details = document.createElement('pre'); details.textContent = JSON.stringify(value, null, 2);
  output.append(details);
}
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
    signedIn = true;
    await csrf();
    log('Browser session is valid.');
    await refreshOrganizations();
  } catch (error) {
    show('session', 'Not signed in. Click “Sign in with GitHub”.');
    $('session').className = 'error';
    signedIn = false; organizationId = null; tenantId = null; selectedOrg = null; cancelOperation();
    log(error.message, true);
  }
  controls();
}
$('login').onclick = () => { window.location.href = '/auth/github/start?next=%2F'; };
$('me').onclick = refreshSession;
$('link-coder').onclick = async () => {
  try {
    const result = await mutate('/api/v1/auth/coder/start');
    window.location.assign(result.authorization_url);
  } catch (error) { show('coder-account', error.message); }
};
$('coder-status').onclick = async () => {
  try { show('coder-account', await request('/api/v1/auth/coder')); }
  catch (error) { show('coder-account', error.message); }
};
$('create-org').onclick = async () => {
  try {
    await csrf();
    const data = await request('/api/v1/organizations', { method: 'POST', headers: {
      'Content-Type': 'application/json', 'X-CSRF-Token': csrfToken, 'Idempotency-Key': `ui-org-${Date.now()}`
    }, body: JSON.stringify({ slug: $('slug').value.trim(), display_name: $('display').value.trim() }) });
    organizationId = data.resource_id;
    $('start').disabled = false; $('status').disabled = false;
    show('org-output', data); log(`Organization queued: ${organizationId}`);
    await refreshOrganizations(organizationId);
    tenantId = null; await refreshTenants(); watchOperation(data.operation_id);
  } catch (error) { log(error.message, true); show('org-output', error.message); }
};
$('start').onclick = async () => {
  try {
    await csrf();
    connection = await request(`/api/v1/organizations/${organizationId}/github/connect`, { method: 'POST', headers: {
      'Content-Type': 'application/json', 'X-CSRF-Token': csrfToken, 'Idempotency-Key': `ui-connect-${Date.now()}`
    }, body: JSON.stringify({ flow_type: 'both' }) });
    showConnection(connection); log('Connection flow created. Open both links.');
  } catch (error) { log(error.message, true); show('connection-output', error.message); }
};
$('status').onclick = async () => {
  try { show('connection-output', await request(`/api/v1/organizations/${organizationId}/github/connections`)); log('Connection status refreshed.'); }
  catch (error) { log(error.message, true); }
};
$('refresh-orgs').onclick = () => action(async () => { await refreshOrganizations(); if (organizationId) await refreshTenants(); }, 'org-output');
$('promote-release').onclick = () => action(async () => {
  const result = await mutate(`/api/v1/platform/organizations/${organizationId}/template-release/promote-configured`);
  show('org-output', result);
  if (result.operation_id) {
    watchOperation(result.operation_id);
    log(`Template release promotion queued: ${result.release_id}`);
  } else {
    log(`Configured template release already promoted: ${result.release_id}`);
  }
}, 'org-output');
$('organizations').onchange = () => action(async () => {
  cancelOperation(); tenantId = null; organizationId = $('organizations').value || null;
  show('tenant-output', 'No tenant selected.'); show('operation-output', 'No operation selected.');
  show('connection-output', 'Refresh connection status for this organization.');
  await refreshOrganizations(organizationId);
  if (organizationId) await refreshTenants();
}, 'org-output');
$('refresh-tenants').onclick = () => action(() => refreshTenants(), 'tenant-output');
$('tenants').onchange = () => action(async () => {
  cancelOperation(); tenantId = $('tenants').value || null; selectedTenant = null;
  show('operation-output', 'No operation selected.');
  if (tenantId) { const status = await tenantStatus(); if (status.latest_operation) watchOperation(status.latest_operation.id); }
  else show('tenant-output', 'No tenant selected.');
}, 'tenant-output');
$('create-tenant').onclick = () => action(async () => {
  const repositoryUrl = $('repository-url').value.trim(), fleet = Number($('fleet').value), name = $('tenant-name').value.trim();
  if (!repositoryUrl || !Number.isSafeInteger(fleet) || fleet < 1 || fleet > 2147483647 || !name) throw new Error('Enter a GitHub repository URL, a tenant name, and a positive whole-number fleet size.');
  const data = await mutate(`/api/v1/organizations/${organizationId}/tenants`, { repository_url: repositoryUrl, name, fleet });
  tenantId = data.tenant_id; show('tenant-output', data);
  log(`Tenant creation queued: ${tenantId}`); await refreshTenants(tenantId); watchOperation(data.operation_id);
}, 'tenant-output');
for (const verb of ['start', 'stop']) $(`${verb}-tenant`).onclick = () => action(async () => {
  const data = await mutate(`/api/v1/organizations/${organizationId}/tenants/${tenantId}/${verb}`);
  show('tenant-output', data); log(`Tenant ${verb} queued.`); watchOperation(data.operation_id);
}, 'tenant-output');
$('delete-tenant').onclick = () => action(async () => {
  if (!selectedTenant || !tenantId) throw new Error('Select a tenant to delete.');
  const confirmed = window.confirm(
    `Delete tenant “${selectedTenant.slug}”? This revokes its credentials, removes its Coder workspaces, and frees its provisioning capacity.`,
  );
  if (!confirmed) return;
  const deletedTenant = selectedTenant.slug;
  const data = await destroy(`/api/v1/organizations/${organizationId}/tenants/${tenantId}`);
  show('tenant-output', data);
  log(`Tenant deletion queued: ${deletedTenant}.`);
  watchOperation(data.operation_id);
}, 'tenant-output');
$('tenant-status').onclick = () => action(async () => { const status = await tenantStatus(); if (status.latest_operation) watchOperation(status.latest_operation.id); }, 'tenant-output');
$('inspect-operation').onclick = () => action(async () => {
  const id = $('operation-id').value.trim();
  if (!id) throw new Error('Enter an operation ID.');
  watchOperation(id);
}, 'operation-output');
$('retry-operation').onclick = () => action(async () => {
  const id = $('operation-id').value.trim();
  const result = await mutate(`/api/v1/operations/${encodeURIComponent(id)}/retry`);
  watchOperation(result.operation_id);
}, 'operation-output');
window.addEventListener('pagehide', cancelOperation);
for (const id of ['create-org', 'start', 'status', 'me']) {
  const handler = $(id).onclick;
  $(id).onclick = () => action(handler);
}
controls(); refreshSession();
</script>
</body>
</html>"###,
    )
}
