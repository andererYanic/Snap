const path = require('path');
const { getDefaultConfig, mergeConfig } = require('@react-native/metro-config');

// Gemeinsame Sprachdateien mit der Desktop-App (apps/desktop/locales).
const locales = path.resolve(__dirname, '../../desktop/locales');

/** @type {import('@react-native/metro-config').MetroConfig} */
const config = {
  watchFolders: [locales],
};

module.exports = mergeConfig(getDefaultConfig(__dirname), config);
