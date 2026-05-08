#!/usr/bin/env node
/**
 * OpenFlows - Autonomous AI Development Team
 * 
 * This wrapper handles:
 * 1. Automatic proxy management (for LLM API translation if needed)
 * 2. Environment configuration
 * 3. Graceful startup and shutdown
 * 
 * Users don't need to know about proxies - everything is handled automatically.
 */
const { spawn, execSync, fork } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');
const http = require('http');

const binaryPath = path.join(__dirname, '..', 'bin', 'agentflow-bin');
const PROXY_PORT = process.env.PROXY_PORT || 8765;
const PROXY_STARTUP_TIMEOUT = 5000;

// Check if a port is in use
function isPortInUse(port) {
    return new Promise((resolve) => {
        const req = http.request({
            method: 'GET',
            hostname: 'localhost',
            port: port,
            path: '/health',
            timeout: 500
        }, (res) => {
            resolve(res.statusCode === 200);
        });
        req.on('error', () => resolve(false));
        req.on('timeout', () => {
            req.destroy();
            resolve(false);
        });
        req.end();
    });
}

// Check if user needs proxy (for LLM API translation if needed)
function needsProxy() {
    // If user explicitly set PROXY_URL, respect it
    if (process.env.PROXY_URL) {
        return { needed: false, reason: 'PROXY_URL already set' };
    }
    
    // If user has Anthropic API key, they can use it directly
    if (process.env.ANTHROPIC_API_KEY) {
        return { needed: false, reason: 'Anthropic direct mode' };
    }
    
    // Fireworks users need proxy for Claude CLI agents (no native Anthropic endpoint)
    if (process.env.FIREWORKS_API_KEY) {
        return { needed: true, reason: 'Fireworks requires proxy for Claude CLI compatibility' };
    }
    
    // If user has Gateway config but no direct keys, they need proxy
    if (process.env.GATEWAY_URL || process.env.GATEWAY_API_KEY) {
        return { needed: true, reason: 'Gateway configured, no direct keys' };
    }
    
    // No API config at all - let the binary handle the error
    return { needed: false, reason: 'No API config - will error in binary' };
}

// Start the built-in proxy (anthropic-proxy binary)
async function startProxy() {
    console.log('[openflows] Starting built-in API proxy...');
    
    let proxyBinary = path.join(__dirname, '..', 'bin', 'anthropic-proxy-bin');
    
    // Check if proxy binary exists
    if (!fs.existsSync(proxyBinary)) {
        // Try alternative location
        const altProxy = path.join(__dirname, '..', 'bin', 'anthropic-proxy');
        if (!fs.existsSync(altProxy)) {
            console.log('[openflows] No built-in proxy found, skipping proxy startup');
            return null;
        }
        proxyBinary = altProxy;
    }
    
    const proxy = spawn(proxyBinary, [], {
        env: {
            ...process.env,
            PORT: PROXY_PORT.toString(),
            RUST_LOG: process.env.RUST_LOG || 'info'
        },
        stdio: ['ignore', 'pipe', 'pipe']
    });
    
    let proxyReady = false;
    
    return new Promise((resolve, reject) => {
        const timeout = setTimeout(() => {
            if (!proxyReady) {
                console.warn('[openflows] Proxy startup timeout, continuing without proxy');
                resolve(null);
            }
        }, PROXY_STARTUP_TIMEOUT);
        
        proxy.stdout.on('data', (data) => {
            const line = data.toString();
            if (line.includes('listening') || line.includes('Proxy') || line.includes('started')) {
                proxyReady = true;
                clearTimeout(timeout);
                console.log(`[openflows] ✓ Proxy started on port ${PROXY_PORT}`);
                resolve(proxy);
            }
        });
        
        proxy.stderr.on('data', (data) => {
            const line = data.toString();
            // Log proxy errors but don't fail
            if (line.includes('ERROR') || line.includes('error')) {
                console.error('[openflows proxy]', line.trim());
            }
        });
        
        proxy.on('error', (err) => {
            clearTimeout(timeout);
            console.warn(`[openflows] Proxy failed to start: ${err.message}`);
            resolve(null);
        });
        
        proxy.on('exit', (code) => {
            if (!proxyReady) {
                clearTimeout(timeout);
                resolve(null);
            } else {
                console.log(`[openflows] Proxy exited with code ${code}`);
            }
        });
    });
}

// Clean up function
let cleanupCalled = false;
function cleanup(proxy, signal = 'SIGTERM') {
    if (cleanupCalled) return;
    cleanupCalled = true;
    
    if (proxy) {
        console.log('[openflows] Stopping proxy...');
        try {
            proxy.kill(signal);
        } catch (err) {
            // Ignore cleanup errors
        }
    }
}

// Main entry point
async function main() {
    const args = process.argv.slice(2);
    
    // Handle special commands
    if (args[0] === '--help' || args[0] === '-h') {
        console.log(`
OpenFlows - Autonomous AI Development Team

Usage:
  openflows [options]

Options:
  --help, -h        Show this help
  --version, -v     Show version
  --no-proxy        Disable automatic proxy startup
  --proxy-only      Start only the proxy (for testing)

Commands:
  openflows-setup     Guided setup wizard
  openflows-dashboard Live monitoring TUI
  openflows-doctor    Diagnostic checks

 Environment Variables:
  ANTHROPIC_API_KEY   Use Anthropic directly (no proxy needed)
  FIREWORKS_API_KEY   Use Fireworks AI (proxy auto-starts for Claude CLI)
  GATEWAY_URL         Custom gateway URL (requires proxy)
  GATEWAY_API_KEY     Custom gateway API key
  PROXY_PORT          Port for built-in proxy (default: 8765)

Examples:
  # Quick start with Anthropic (no proxy needed)
  ANTHROPIC_API_KEY=your-key openflows

  # Use Fireworks (proxy auto-starts for Claude CLI agents)
  FIREWORKS_API_KEY=your-key openflows

  # Use custom gateway (proxy auto-starts)
  GATEWAY_URL=https://your-gateway.com/v1 \\
  GATEWAY_API_KEY=your-key openflows

Documentation: https://openflows.dev
`);
        process.exit(0);
    }
    
    if (args[0] === '--version' || args[0] === '-v') {
        try {
            const pkg = require('../package.json');
            console.log(`openflows v${pkg.version}`);
        } catch {
            console.log('openflows (version unknown)');
        }
        process.exit(0);
    }
    
    // Skip proxy if --no-proxy flag
    const skipProxy = args.includes('--no-proxy');
    
    // Start proxy only mode
    if (args[0] === '--proxy-only') {
        const proxy = await startProxy();
        if (proxy) {
            console.log(`[openflows] Proxy running on http://localhost:${PROXY_PORT}`);
            console.log('[openflows] Press Ctrl+C to stop');
            
            // Keep running until killed
            process.on('SIGINT', () => cleanup(proxy, 'SIGINT'));
            process.on('SIGTERM', () => cleanup(proxy, 'SIGTERM'));
        } else {
            console.error('[openflows] Failed to start proxy');
            process.exit(1);
        }
        return;
    }
    
    // Check if we need to start proxy
    let proxy = null;
    let env = { ...process.env };
    
    if (!skipProxy) {
        const { needed, reason } = needsProxy();
        
        if (needed) {
            console.log(`[openflows] ${reason} - starting proxy...`);
            
            // Check if proxy is already running
            const proxyRunning = await isPortInUse(PROXY_PORT);
            
            if (proxyRunning) {
                console.log(`[openflows] ✓ Proxy already running on port ${PROXY_PORT}`);
            } else {
                proxy = await startProxy();
                if (proxy) {
                    // Set PROXY_URL for the main binary
                    env.PROXY_URL = `http://localhost:${PROXY_PORT}/v1`;
                }
            }
        } else {
            console.log(`[openflows] Mode: ${reason}`);
        }
    }
    
    // Spawn the main binary
    const proc = spawn(binaryPath, args, {
        env,
        stdio: 'inherit'
    });
    
    // Handle signals
    process.on('SIGINT', () => {
        cleanup(proxy, 'SIGINT');
        proc.kill('SIGINT');
    });
    
    process.on('SIGTERM', () => {
        cleanup(proxy, 'SIGTERM');
        proc.kill('SIGTERM');
    });
    
    // Handle exit
    proc.on('exit', (code) => {
        cleanup(proxy);
        process.exit(code || 0);
    });
    
    proc.on('error', (err) => {
        console.error('[openflows] Failed to start:', err.message);
        cleanup(proxy);
        process.exit(1);
    });
}

main().catch(err => {
    console.error('[openflows] Error:', err.message);
    process.exit(1);
});
