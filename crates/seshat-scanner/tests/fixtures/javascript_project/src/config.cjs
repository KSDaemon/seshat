// CommonJS config module — .cjs forces CommonJS module system
const path = require('path');
const { readFileSync } = require('fs');

const DEFAULT_PORT = 3000;
const DEFAULT_HOST = 'localhost';

function loadConfig(configPath) {
    const fullPath = path.resolve(configPath);
    const raw = readFileSync(fullPath, 'utf8');
    return JSON.parse(raw);
}

function getDefaultConfig() {
    return {
        port: DEFAULT_PORT,
        host: DEFAULT_HOST,
    };
}

class ConfigManager {
    constructor(config) {
        this.config = config || getDefaultConfig();
    }

    get(key) {
        return this.config[key];
    }
}

module.exports = { loadConfig, getDefaultConfig, ConfigManager };
