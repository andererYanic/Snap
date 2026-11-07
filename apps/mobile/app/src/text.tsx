// Text und Eingabefelder der App mit der gewählten Schrift (Standard: Inconsolata). Android
// registriert die Schrift unter „SnapFont“ (siehe Fonts.kt); eigene Angaben im `style` (z. B.
// `fontFamily: 'monospace'` für Code) haben Vorrang. `ref` wird (React 19) normal durchgereicht.
import type { ComponentProps } from 'react';
import { StyleSheet, Text as RNText, TextInput as RNTextInput } from 'react-native';

export const FONT = 'SnapFont';
const base = StyleSheet.create({ font: { fontFamily: FONT } });

export function Text({ style, ...rest }: ComponentProps<typeof RNText>) {
  return <RNText {...rest} style={[base.font, style]} />;
}

export function TextInput({ style, ...rest }: ComponentProps<typeof RNTextInput>) {
  return <RNTextInput {...rest} style={[base.font, style]} />;
}
