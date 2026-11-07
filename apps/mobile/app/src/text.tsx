// Text und Eingabefelder der App mit der gewählten Schrift (Standard: Inconsolata). Android
// registriert die Schrift unter einem Namen (siehe Fonts.kt); bei jeder Änderung gibt es einen
// neuen Namen, den alle Texte über den Kontext übernehmen – ohne die Oberfläche neu aufzubauen.
// Eigene Angaben im `style` (z. B. `fontFamily: 'monospace'` für Code) haben Vorrang.
import { createContext, useContext, type ComponentProps } from 'react';
import { Text as RNText, TextInput as RNTextInput } from 'react-native';

export const FONT = 'SnapFont';
export const FontContext = createContext<string>(FONT);

export function Text({ style, ...rest }: ComponentProps<typeof RNText>) {
  const fontFamily = useContext(FontContext);
  return <RNText {...rest} style={[{ fontFamily }, style]} />;
}

export function TextInput({ style, ...rest }: ComponentProps<typeof RNTextInput>) {
  const fontFamily = useContext(FontContext);
  return <RNTextInput {...rest} style={[{ fontFamily }, style]} />;
}
