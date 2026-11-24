// Learn more: https://docs.expo.dev/guides/monorepos/
const path = require("path");
const { getDefaultConfig } = require("expo/metro-config");

const projectRoot = __dirname;
const monorepoRoot = path.resolve(projectRoot, "../..");

const config = getDefaultConfig(projectRoot);

// Watch shared @gitpulse/core workspace package
config.watchFolders = [path.resolve(monorepoRoot, "packages/gitpulse-core")];

config.resolver.nodeModulesPaths = [
  path.resolve(projectRoot, "node_modules"),
  path.resolve(monorepoRoot, "node_modules"),
];

module.exports = config;
