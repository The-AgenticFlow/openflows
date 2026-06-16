#!/usr/bin/env node
const { spawn } = require('child_process');
const path = require('path');
const binaryPath = path.join(__dirname, '..', 'bin', 'openflows-setup-bin');
const proc = spawn(binaryPath, process.argv.slice(2), { stdio: 'inherit' });
proc.on('exit', (code) => process.exit(code));
