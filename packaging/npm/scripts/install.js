#!/usr/bin/env node
/**
 * Post-install script for @the-agenticflow/openflows
 * 
 * This script:
 * 1. Downloads the correct pre-built binary for the current platform
 * 2. Installs mcp-proxy (required for GitHub MCP connectivity)
 * 3. Verifies all dependencies are ready
 */
const https = require('https');
const http = require('http');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { execSync, spawn } = require('child_process');

const REPO = 'The-AgenticFlow/AgentFlow';
const BIN_DIR = path.join(__dirname, '..', 'bin');
const POSTINSTALL_LOG = path.join(__dirname, '..', '.postinstall-done');

function detectPlatform() {
    const platform = os.platform();
    const arch = os.arch();

    let osPart, archPart;

    switch (platform) {
        case 'darwin':
            osPart = 'apple-darwin';
            break;
        case 'linux':
            // Check for musl
            try {
                const ldd = execSync('ldd --version 2>&1', { encoding: 'utf8' });
                if (ldd.includes('musl')) {
                    osPart = 'unknown-linux-musl';
                } else {
                    osPart = 'unknown-linux-gnu';
                }
            } catch {
                osPart = 'unknown-linux-gnu';
            }
            break;
        default:
            console.error(`Unsupported platform: ${platform}`);
            process.exit(1);
    }

    switch (arch) {
        case 'x64':
            archPart = 'x86_64';
            break;
        case 'arm64':
            archPart = 'aarch64';
            break;
        default:
            console.error(`Unsupported architecture: ${arch}`);
            process.exit(1);
    }

    return `${archPart}-${osPart}`;
}

function download(url, dest) {
    return new Promise((resolve, reject) => {
        const parsed = new URL(url);
        const client = parsed.protocol === 'https:' ? https : http;
        const file = fs.createWriteStream(dest);

        client.get(url, (response) => {
            if (response.statusCode === 302 || response.statusCode === 301) {
                download(response.headers.location, dest).then(resolve).catch(reject);
                return;
            }
            if (response.statusCode !== 200) {
                reject(new Error(`HTTP ${response.statusCode}: ${response.statusMessage}`));
                return;
            }
            response.pipe(file);
            file.on('finish', () => {
                file.close();
                resolve();
            });
        }).on('error', (err) => {
            fs.unlink(dest, () => {});
            reject(err);
        });
    });
}

function extractTarGz(tarPath, destDir) {
    return new Promise((resolve, reject) => {
        const tar = require('child_process').spawn('tar', ['-xzf', tarPath, '-C', destDir, '--strip-components=1']);
        tar.on('close', (code) => {
            if (code === 0) resolve();
            else reject(new Error(`tar exited with code ${code}`));
        });
    });
}

/**
 * Install mcp-proxy - required for GitHub MCP connectivity
 * 
 * mcp-proxy is a Python tool from PyPI (sparfenyuk/mcp-proxy)
 * It bridges stdio to HTTP MCP servers like GitHub Copilot's MCP endpoint.
 * 
 * Strategy:
 * 1. Check if mcp-proxy is already available
 * 2. Install via uv (fast) or pipx (alternative)
 * 3. Fall back to Docker mode instructions if Python tools unavailable
 */
async function ensureMcpProxy() {
    console.log(`[openflows] Checking mcp-proxy installation...`);
    
    // Check if already installed
    try {
        execSync('which mcp-proxy', { stdio: 'pipe' });
        console.log(`[openflows] ✓ mcp-proxy already installed`);
        return true;
    } catch {
        // Not installed, proceed with installation
    }
    
    console.log(`[openflows] Installing mcp-proxy (required for GitHub MCP)...`);
    
    // Try uv first (fastest)
    try {
        execSync('which uv', { stdio: 'pipe' });
        console.log(`[openflows] Installing via uv...`);
        execSync('uv tool install mcp-proxy', {
            stdio: 'inherit',
            timeout: 120000 
        });
        console.log(`[openflows] ✓ mcp-proxy installed via uv`);
        return true;
    } catch (err) {
        // uv failed or not available
    }
    
    // Try pipx as alternative
    try {
        execSync('which pipx', { stdio: 'pipe' });
        console.log(`[openflows] Installing via pipx...`);
        execSync('pipx install mcp-proxy', {
            stdio: 'inherit',
            timeout: 120000 
        });
        console.log(`[openflows] ✓ mcp-proxy installed via pipx`);
        return true;
    } catch (err) {
        // pipx failed or not available
    }
    
    // Try pip3 as last resort
    try {
        execSync('which pip3', { stdio: 'pipe' });
        console.log(`[openflows] Installing via pip3...`);
        execSync('pip3 install --user mcp-proxy', {
            stdio: 'inherit',
            timeout: 120000 
        });
        console.log(`[openflows] ✓ mcp-proxy installed via pip3`);
        return true;
    } catch (err) {
        // pip3 failed or not available
    }
    
    console.warn(`[openflows] ⚠ Could not install mcp-proxy automatically.`);
    console.warn(`[openflows]   Please install manually:`);
    console.warn(`[openflows]     uv tool install mcp-proxy`);
    console.warn(`[openflows]   Or use Docker mode:`);
    console.warn(`[openflows]     export GITHUB_MCP_TYPE=docker`);
    return false;
}

/**
 * Check if essential tools are available
 */
function checkPrerequisites() {
    const checks = [
        { name: 'git', cmd: 'git --version' },
        { name: 'node', cmd: 'node --version' },
    ];
    
    console.log(`[openflows] Checking prerequisites...`);
    
    let allPassed = true;
    for (const check of checks) {
        try {
            execSync(check.cmd, { stdio: 'pipe' });
            console.log(`[openflows] ✓ ${check.name} available`);
        } catch {
            console.warn(`[openflows] ✗ ${check.name} not found - please install it`);
            allPassed = false;
        }
    }
    
    return allPassed;
}

async function main() {
    // Skip postinstall if already done (e.g., during npm link)
    if (fs.existsSync(POSTINSTALL_LOG)) {
        const age = Date.now() - fs.statSync(POSTINSTALL_LOG).mtimeMs;
        if (age < 60000) { // Less than 1 minute old
            console.log(`[openflows] Postinstall already completed, skipping...`);
            return;
        }
    }

    const platform = detectPlatform();
    console.log(``);
    console.log(`╔══════════════════════════════════════════════╗`);
    console.log(`║     OpenFlows Installation                    ║`);
    console.log(`║     Autonomous AI Development Team            ║`);
    console.log(`╚══════════════════════════════════════════════╝`);
    console.log(``);
    console.log(`[openflows] Platform: ${platform}`);

    // Ensure bin directory exists
    if (!fs.existsSync(BIN_DIR)) {
        fs.mkdirSync(BIN_DIR, { recursive: true });
    }

    // Get latest release tag with better error handling
    let tag;
    try {
        tag = await new Promise((resolve, reject) => {
            const req = https.get(`https://api.github.com/repos/${REPO}/releases/latest`, {
                headers: { 
                    'User-Agent': 'openflows-npm-installer',
                    'Accept': 'application/vnd.github.v3+json'
                }
            }, (res) => {
                if (res.statusCode !== 200) {
                    reject(new Error(`GitHub API returned ${res.statusCode}`));
                    return;
                }
                let data = '';
                res.on('data', (chunk) => data += chunk);
                res.on('end', () => {
                    try {
                        const json = JSON.parse(data);
                        if (!json.tag_name) {
                            reject(new Error('No tag_name in release response'));
                        } else {
                            resolve(json.tag_name);
                        }
                    } catch (parseErr) {
                        reject(new Error(`Failed to parse release info: ${parseErr.message}`));
                    }
                });
            });
            req.on('error', reject);
            req.setTimeout(30000, () => {
                req.destroy();
                reject(new Error('GitHub API request timeout'));
            });
        });
    } catch (apiErr) {
        console.error(`[openflows] GitHub API error: ${apiErr.message}`);
        console.error('[openflows] Falling back to latest known version: v0.1.6');
        tag = 'v0.1.6';
    }

    const archiveName = `openflows-${tag}-${platform}.tar.gz`;
    const downloadUrl = `https://github.com/${REPO}/releases/download/${tag}/${archiveName}`;
    // Use package's temp directory instead of system /tmp to avoid permission issues
    const tmpDir = path.join(__dirname, '..', '.tmp');
    if (!fs.existsSync(tmpDir)) {
        fs.mkdirSync(tmpDir, { recursive: true });
    }
    const tmpFile = path.join(tmpDir, archiveName);

    try {
        console.log(`[openflows] Downloading binary for ${platform}...`);
        await download(downloadUrl, tmpFile);
        await extractTarGz(tmpFile, BIN_DIR);
    } catch (err) {
        // For x86_64 Linux, try musl fallback
        if (platform === 'x86_64-unknown-linux-gnu') {
            const muslArchiveName = `openflows-${tag}-x86_64-unknown-linux-musl.tar.gz`;
            const muslDownloadUrl = `https://github.com/${REPO}/releases/download/${tag}/${muslArchiveName}`;
            const muslTmpFile = path.join(tmpDir, muslArchiveName);
            console.log(`[openflows] Trying musl fallback...`);
            await download(muslDownloadUrl, muslTmpFile);
            await extractTarGz(muslTmpFile, BIN_DIR);
            fs.unlinkSync(muslTmpFile);
        } else {
            throw err;
        }
    }

    // Rename binaries to match expected names
    const binaries = ['agentflow', 'agentflow-setup', 'agentflow-dashboard', 'agentflow-doctor', 'anthropic-proxy'];
    for (const bin of binaries) {
        const src = path.join(BIN_DIR, bin);
        const dst = path.join(BIN_DIR, `${bin}-bin`);
        if (fs.existsSync(src)) {
            fs.renameSync(src, dst);
            fs.chmodSync(dst, 0o755);
        }
    }
    
    // Also ensure anthropic-proxy is executable even if not renamed
    const proxyBin = path.join(BIN_DIR, 'anthropic-proxy');
    if (fs.existsSync(proxyBin)) {
        fs.chmodSync(proxyBin, 0o755);
    }

    if (fs.existsSync(tmpFile)) {
        fs.unlinkSync(tmpFile);
    }
    // Clean up temp directory if empty
    try {
        if (fs.existsSync(tmpDir) && fs.readdirSync(tmpDir).length === 0) {
            fs.rmdirSync(tmpDir);
        }
    } catch (cleanupErr) {
        // Ignore cleanup errors
    }

    console.log(`[openflows] ✓ Binaries installed`);

    // Install mcp-proxy
    await ensureMcpProxy();
    
    // Check prerequisites
    checkPrerequisites();

    // Mark postinstall as done
    fs.writeFileSync(POSTINSTALL_LOG, new Date().toISOString());

    console.log(``);
    console.log(`╔══════════════════════════════════════════════╗`);
    console.log(`║     Installation Complete!                    ║`);
    console.log(`╚══════════════════════════════════════════════╝`);
    console.log(``);
    console.log(`  Available commands:`);
    console.log(`    openflows           - Start orchestration`);
    console.log(`    openflows-setup     - Guided setup wizard`);
    console.log(`    openflows-dashboard - Live monitoring TUI`);
    console.log(`    openflows-doctor    - Diagnostic checks`);
    console.log(``);
    console.log(`  Quick start:`);
    console.log(`    1. openflows-setup     # Configure API keys`);
    console.log(`    2. openflows           # Start the autonomous team`);
    console.log(``);
    console.log(`  Docs: https://openflows.dev`);
    console.log(``);
}

main().catch(err => {
    console.error('[openflows] Installation failed:', err.message);
    process.exit(1);
});
