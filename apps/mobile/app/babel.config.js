module.exports = {
  presets: ['module:@react-native/babel-preset'],
  // Animationen (Reanimated 4): Worklets laufen auf dem UI-Thread – muss als letztes Plugin stehen.
  plugins: ['react-native-worklets/plugin'],
};
