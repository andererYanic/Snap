/**
 * @format
 */

import { AppRegistry } from 'react-native';
import App from './App';
import { name as appName } from './app.json';

AppRegistry.registerComponent(appName, () => App);
// Dateiauswahl für andere Apps (PickerActivity)
import Picker from './src/Picker';
AppRegistry.registerComponent('SnapPicker', () => Picker);
