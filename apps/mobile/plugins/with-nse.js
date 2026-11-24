// CommonJS entry shim for Expo CLI / @expo/config-plugins resolution.
// Delegates directly to the TypeScript source of truth (`./with-nse.ts`).
const path = require("node:path");
const { loadModuleSync } = require("@expo/require-utils");

module.exports = loadModuleSync(path.join(__dirname, "with-nse.ts"));
