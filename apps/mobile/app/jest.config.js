module.exports = {
  preset: '@react-native/jest-preset',
  // Bibliotheken, die als ES-Module ausgeliefert werden, ebenfalls übersetzen
  transformIgnorePatterns: [
    'node_modules/(?!((jest-)?react-native|@react-native(-community)?|react-native-(gesture-handler|reanimated|worklets|video|safe-area-context))/)',
  ],
  setupFiles: ['./jest.setup.js'],
};
