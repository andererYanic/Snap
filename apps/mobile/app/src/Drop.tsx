// Snap Drop – Oberfläche: Senden an Geräte in der Nähe, Annahme eingehender Angebote,
// Kurzanfrage für den Offline-Weg und die Einstellungsseite (Sichtbarkeit, Kontakte, Sperren).
// Grundsätze: docs/DESIGN.md; Sicherheit: docs/DROP.md.
import { useCallback, useEffect, useRef, useState } from 'react';
import { Alert, BackHandler, Image, Modal, Pressable, ScrollView, Share, StyleSheet, Switch, TextInput, View } from 'react-native';
import Animated from 'react-native-reanimated';
import * as core from './core';
import * as drop from './drop';
import { locale, t } from './i18n';
import { Icon, type IconName } from './Icon';
import { fadeIn, m, sheetIn } from './motion';
import { Text } from './text';
import type { Theme } from './theme';
import { SectionLabel, themed } from './ui';

const size = (n: number) => core.humanSize(n, locale());
/** Rechnername eines Links (für die Anzeige; der volle Link steht darunter). */
export function hostOf(url: string): string {
  const m = /^https?:\/\/([^/?#]+)/i.exec(url);
  return m ? m[1].toLowerCase() : url;
}
const filesLabel = (n: number) => (n === 1 ? t('1 Datei') : t('{n} Dateien', { n }));

function kindIcon(kind: drop.NearbyPeer['kind']): IconName {
  return kind === 'laptop' || kind === 'desktop' ? 'laptop' : kind === 'tablet' ? 'tablet' : 'phone';
}

/** Kreis mit Anfangsbuchstaben; Kontakte in der Akzentfarbe, Fremde neutral. */
export function Avatar({ name, contact, theme, size: d = 44 }: { name: string; contact?: boolean; theme: Theme; size?: number }) {
  const letter = (name.trim()[0] ?? '?').toUpperCase();
  return (
    <View
      style={{
        width: d,
        height: d,
        borderRadius: d / 2,
        justifyContent: 'center',
        alignItems: 'center',
        backgroundColor: (contact ? theme.accentSoft : theme.surface2) as string,
      }}
    >
      <Text style={{ color: (contact ? theme.accent : theme.text2) as string, fontSize: d * 0.42, fontWeight: '700' }}>{letter}</Text>
    </View>
  );
}

/** Blatt von unten wie `BottomSheet`, aber mit freiem Inhalt. */
function Sheet({ theme, onClose, children }: { theme: Theme; onClose: () => void; children: React.ReactNode }) {
  const s = styles(theme);
  return (
    <Modal visible transparent animationType="none" statusBarTranslucent onRequestClose={onClose}>
      <Animated.View entering={m(fadeIn)} style={s.scrim}>
        <Pressable style={s.fill} onPress={onClose} accessibilityLabel={t('Schließen')} />
      </Animated.View>
      <Animated.View entering={m(sheetIn)} style={s.sheet}>
        <View style={s.grabber} />
        {children}
      </Animated.View>
    </Modal>
  );
}

// ------------------------------------------------------------------ Senden

type SendState = { peer: string; phase: 'waiting' | 'sending' | 'done' | 'failed'; done: number; total: number; error?: string };

export function DropSendSheet({ paths, text = null, theme, onClose, notify }: {
  paths: string[]; text?: string | null; theme: Theme; onClose: () => void; notify: (text: string) => void;
}) {
  const s = styles(theme);
  const [peers, setPeers] = useState<drop.NearbyPeer[]>([]);
  const [state, setState] = useState<SendState | null>(null);
  const [people, setPeople] = useState<drop.Contact[]>([]);
  // Kontakt, der gerade geweckt wird (wird gesendet, sobald er erscheint)
  const [waking, setWaking] = useState<string | null>(null);
  const [codeOpen, setCodeOpen] = useState(false);
  const [code, setCode] = useState('');
  const [codeBusy, setCodeBusy] = useState(false);
  const busy = state?.phase === 'waiting' || state?.phase === 'sending';

  const refresh = useCallback(() => {
    drop.nearby().then(setPeers).catch(() => {});
  }, []);

  useEffect(() => {
    drop.startBrowse();
    drop.wakeAll();
    drop.contacts().then(setPeople).catch(() => {});
    refresh();
    const timer = setInterval(refresh, 1500);
    const off = core.onEvent((ev) => {
      if (ev.type === 'DropNearbyChanged') {
        refresh();
      } else if (ev.type === 'DropProgress' && !ev.incoming) {
        setState((st) => (st ? { ...st, phase: 'sending', done: ev.done_bytes, total: ev.total_bytes } : st));
      }
    });
    return () => {
      clearInterval(timer);
      off();
      drop.stopWake();
      drop.stopBrowse();
    };
  }, [refresh]);

  useEffect(() => {
    const h = BackHandler.addEventListener('hardwareBackPress', () => {
      onClose();
      return true;
    });
    return () => h.remove();
  }, [onClose]);

  const sendTo = async (p: drop.NearbyPeer) => {
    if (busy) {
      return;
    }
    core.haptic('select');
    setState({ peer: p.id, phase: 'waiting', done: 0, total: 0 });
    try {
      const r = await drop.send(p, paths, text);
      if (r.ok) {
        // Hinweis (mit „Als Kontakt speichern“) kommt über das Ereignis `DropFinished`.
        setState({ peer: p.id, phase: 'done', done: 1, total: 1 });
        core.haptic('confirm');
        setTimeout(onClose, 900);
      } else {
        setState({ peer: p.id, phase: 'failed', done: 0, total: 0, error: r.error ?? t('Fehlgeschlagen') });
      }
    } catch (e: any) {
      setState({ peer: p.id, phase: 'failed', done: 0, total: 0, error: String(e?.message ?? e) });
    }
  };

  // Geweckter Kontakt taucht auf → sofort senden
  useEffect(() => {
    if (!waking) {
      return;
    }
    const p = peers.find((x) => x.contact_key === waking && !x.offline);
    if (p) {
      setWaking(null);
      drop.stopWake();
      sendTo(p);
    }
    // sendTo ändert sich bei jedem Zeichnen; entscheidend sind nur Kontakt und Liste
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [waking, peers]);

  const wake = async (c: drop.Contact) => {
    if (busy || waking) {
      return;
    }
    core.haptic('select');
    const id = `contact:${c.key}`;
    // Adresse bekannt: direkt von überall senden (die Gegenseite nimmt wie immer an)
    if (c.remote) {
      setState({ peer: id, phase: 'waiting', done: 0, total: 0 });
      try {
        const r = await drop.sendToContact(c.key, paths, text);
        if (r.ok) {
          setState({ peer: id, phase: 'done', done: 1, total: 1 });
          core.haptic('confirm');
          setTimeout(onClose, 900);
          return;
        }
        setState({ peer: id, phase: 'failed', done: 0, total: 0, error: r.error ?? t('Fehlgeschlagen') });
        if (/abgelehnt|declined/i.test(r.error ?? '')) {
          return;
        }
      } catch (e: any) {
        setState({ peer: id, phase: 'failed', done: 0, total: 0, error: String(e?.message ?? e) });
      }
    }
    // Sonst (oder nicht erreichbar): per Bluetooth wecken, falls in der Nähe
    if (!(await drop.wakeContact(c.key))) {
      if (!c.remote) {
        notify(t('Bluetooth ist aus oder nicht erlaubt – „{name}“ lässt sich nicht wecken.', { name: c.name }));
      }
      return;
    }
    setWaking(c.key);
    // Nach 90 s ohne Antwort aufgeben
    setTimeout(() => setWaking((w) => (w === c.key ? null : w)), 90_000);
  };

  const connectCode = async () => {
    setCodeBusy(true);
    try {
      const p = await drop.addCode(code.trim());
      setCodeOpen(false);
      setCode('');
      setPeers((old) => [p, ...old.filter((x) => x.id !== p.id)]);
    } catch (e: any) {
      notify(String(e?.message ?? e));
    } finally {
      setCodeBusy(false);
    }
  };

  const textOnly = paths.length === 0;
  const subtitle = textOnly
    ? (text ?? '').replace(/\s+/g, ' ').slice(0, 80)
    : paths.length === 1
      ? paths[0].slice(paths[0].lastIndexOf('/') + 1)
      : filesLabel(paths.length);

  return (
    <Sheet theme={theme} onClose={onClose}>
      <View style={s.sheetHead}>
        <Text style={s.sheetTitle}>{t('In der Nähe senden')}</Text>
        <Text style={s.meta} numberOfLines={1}>{subtitle}</Text>
      </View>
      <ScrollView style={s.list} contentContainerStyle={s.listInner}>
        {peers.length === 0 ? (
          <View style={s.empty}>
            <View style={s.emptyIcon}>
              <Icon name="nearby" size={28} color={theme.text2 as string} />
            </View>
            <Text style={s.label}>{t('Suche nach Geräten …')}</Text>
            <Text style={[s.meta, s.center]}>
              {t('Auf dem anderen Gerät Snap öffnen und unter Einstellungen › Snap Drop „Alle“ wählen.')}
            </Text>
          </View>
        ) : (
          peers.map((p) => {
            const mine = state?.peer === p.id ? state : null;
            const name = p.contact ?? p.name ?? t('Snap-Gerät');
            // Text braucht ein gemeinsames Netz (der Offline-Weg trägt nur Dateien)
            const unavailable = textOnly && p.offline;
            const meta = mine?.phase === 'waiting'
              ? t('Wartet auf Annahme …')
              : mine?.phase === 'sending'
                ? `${size(mine.done)} / ${size(mine.total)}`
                : mine?.phase === 'done'
                  ? t('Gesendet')
                  : mine?.phase === 'failed'
                    ? mine.error ?? t('Fehlgeschlagen')
                    : p.contact
                      ? t('Kontakt')
                      : p.offline
                        ? t('Ohne gemeinsames Netz · direkt')
                        : t('In der Nähe');
            return (
              <Pressable
                key={p.id}
                onPress={() => sendTo(p)}
                disabled={busy || unavailable}
                android_ripple={{ color: theme.accentSoft as string }}
                style={[s.peer, ((busy && !mine) || unavailable) && s.dim]}
                accessibilityRole="button"
                accessibilityLabel={t('An {name} senden', { name })}
              >
                <Avatar name={name} contact={!!p.contact} theme={theme} />
                <View style={s.grow}>
                  <View style={s.rowInline}>
                    <Text style={s.label} numberOfLines={1}>{name}</Text>
                    {p.kind ? <Icon name={kindIcon(p.kind)} size={16} color={theme.text3 as string} /> : null}
                  </View>
                  <Text style={[s.meta, mine?.phase === 'failed' && s.dangerText]} numberOfLines={2}>{meta}</Text>
                  {mine?.phase === 'sending' && mine.total > 0 ? (
                    <View style={s.track}>
                      <View style={[s.bar, { width: `${Math.min(100, (mine.done / mine.total) * 100)}%` }]} />
                    </View>
                  ) : null}
                </View>
                {mine?.phase === 'done' ? <Icon name="check" size={22} color={theme.accent as string} /> : null}
              </Pressable>
            );
          })
        )}
        {people.filter((c) => !peers.some((p) => p.contact_key === c.key)).length > 0 ? (
          <>
            <Text style={[s.meta, s.groupLabel]}>{t('Kontakte, nicht in der Nähe')}</Text>
            {people
              .filter((c) => !peers.some((p) => p.contact_key === c.key))
              .map((c) => (
                <Pressable
                  key={c.key}
                  onPress={() => wake(c)}
                  disabled={busy || !!waking}
                  android_ripple={{ color: theme.accentSoft as string }}
                  style={[s.peer, ((busy && state?.peer !== `contact:${c.key}`) || (!!waking && waking !== c.key)) && s.dim]}
                  accessibilityRole="button"
                  accessibilityLabel={c.remote ? t('An {name} senden', { name: c.name }) : t('{name} wecken', { name: c.name })}
                >
                  <Avatar name={c.name} contact theme={theme} />
                  <View style={s.grow}>
                    <Text style={s.label} numberOfLines={1}>{c.name}</Text>
                    <Text style={[s.meta, state?.peer === `contact:${c.key}` && state.phase === 'failed' && s.dangerText]} numberOfLines={2}>
                      {state?.peer === `contact:${c.key}` && state.phase !== 'failed'
                        ? state.phase === 'waiting'
                          ? t('Wartet auf Annahme …')
                          : state.phase === 'sending'
                            ? `${size(state.done)} / ${size(state.total)}`
                            : t('Gesendet')
                        : waking === c.key
                          ? t('Wird geweckt … (Bluetooth, in Reichweite)')
                          : state?.peer === `contact:${c.key}` && state.phase === 'failed'
                            ? state.error ?? t('Fehlgeschlagen')
                            : c.remote
                              ? t('Von überall senden – verschlüsselt, auch übers Internet')
                              : t('Antippen zum Wecken – Snap muss dort nicht offen sein')}
                    </Text>
                  </View>
                </Pressable>
              ))}
          </>
        ) : null}
      </ScrollView>
      {codeOpen ? (
        <View style={s.codeRow}>
          <TextInput
            value={code}
            onChangeText={setCode}
            autoFocus
            autoCapitalize="none"
            autoCorrect={false}
            placeholder="snapdrop1-…"
            placeholderTextColor={theme.text3 as string}
            style={s.input}
            onSubmitEditing={connectCode}
          />
          <Pressable onPress={connectCode} disabled={codeBusy || !code.trim()} style={[s.tonal, (codeBusy || !code.trim()) && s.dim]} accessibilityRole="button">
            <Text style={s.tonalText}>{t('Verbinden')}</Text>
          </Pressable>
        </View>
      ) : (
        <Pressable onPress={() => setCodeOpen(true)} style={s.textBtn} accessibilityRole="button">
          <Text style={s.link}>{t('Mit Code verbinden')}</Text>
        </Pressable>
      )}
    </Sheet>
  );
}

// ------------------------------------------------------------------ Empfangen

/** Eingehendes, im Kern geprüftes Angebot. */
export function OfferSheet({ offer, theme, onDone }: { offer: drop.IncomingOffer; theme: Theme; onDone: () => void }) {
  const s = styles(theme);
  const [png, setPng] = useState<string | null>(null);
  const [reveal, setReveal] = useState(false);
  const answered = useRef(false);

  useEffect(() => {
    if (offer.preview) {
      drop.preview(offer.id).then(setPng).catch(() => {});
    }
    core.haptic('confirm');
  }, [offer.id, offer.preview]);

  const answer = (accept: boolean, block = false) => {
    if (answered.current) {
      return;
    }
    answered.current = true;
    drop.respond(offer.id, accept, block).catch(() => {});
    onDone();
  };

  const name = offer.contact ?? offer.name;
  const shown = offer.files.slice(0, 3);
  const more = offer.files.length - shown.length;
  return (
    <Sheet theme={theme} onClose={() => answer(false)}>
      <View style={s.offer}>
        <View style={s.offerHead}>
          <Avatar name={name} contact={!!offer.contact} theme={theme} size={52} />
          <View style={s.grow}>
            <Text style={s.offerName} numberOfLines={1}>{name}</Text>
            {offer.contact ? (
              <View style={s.rowInline}>
                <Icon name="user" size={14} color={theme.accent as string} />
                <Text style={[s.meta, s.accentText]}>{t('Kontakt')}</Text>
              </View>
            ) : (
              <Text style={s.meta}>{t('Unbekannt – nur annehmen, wenn du die Person kennst.')}</Text>
            )}
          </View>
          {png ? (
            <Pressable onPress={() => setReveal(true)} accessibilityLabel={t('Vorschau anzeigen')}>
              <Image source={{ uri: `data:image/png;base64,${png}` }} blurRadius={reveal ? 0 : 18} style={s.preview} />
            </Pressable>
          ) : null}
        </View>
        <Text style={s.offerText}>
          {offer.files.length === 0
            ? offer.link
              ? t('{name} möchte dir einen Link senden.', { name })
              : t('{name} möchte dir einen Text senden.', { name })
            : t('{name} möchte dir {files} senden ({size}).', { name, files: filesLabel(offer.files.length), size: size(offer.total) })}
        </Text>
        {offer.text ? (
          <View style={s.fileBox}>
            {offer.link ? (
              <>
                <Text style={s.linkHost} numberOfLines={1}>{hostOf(offer.link)}</Text>
                <Text style={s.meta} numberOfLines={2}>{offer.link}</Text>
              </>
            ) : (
              <Text style={s.fileName} numberOfLines={6}>{offer.text}</Text>
            )}
          </View>
        ) : null}
        {offer.files.length > 0 && (
        <View style={s.fileBox}>
          {shown.map((f, i) => (
            <View key={i} style={s.fileRow}>
              <Text style={s.fileName} numberOfLines={1}>{f.name}</Text>
              <Text style={s.fileSize}>{size(f.size)}</Text>
            </View>
          ))}
          {more > 0 ? <Text style={s.meta}>{t('und {n} weitere', { n: more })}</Text> : null}
        </View>
        )}
        {offer.text ? <Text style={s.meta}>{t('Wird nach dem Annehmen in die Zwischenablage kopiert.')}</Text> : null}
        <Text style={s.code} accessibilityLabel={t('Prüfcode {code}', { code: offer.code })}>
          {t('Prüfcode')}  <Text style={s.codeNum}>{offer.code}</Text>
        </Text>
        <View style={s.actions}>
          <Pressable onPress={() => answer(false)} style={[s.tonal, s.grow]} accessibilityRole="button">
            <Text style={s.tonalText}>{t('Ablehnen')}</Text>
          </Pressable>
          <Pressable onPress={() => answer(true)} style={[s.primary, s.grow]} accessibilityRole="button">
            <Text style={s.primaryText}>{t('Annehmen')}</Text>
          </Pressable>
        </View>
        {!offer.contact ? (
          <Pressable onPress={() => answer(false, true)} style={s.textBtn} accessibilityRole="button">
            <Text style={[s.link, s.dangerText]}>{t('Ablehnen und blockieren')}</Text>
          </Pressable>
        ) : null}
      </View>
    </Sheet>
  );
}

/** Kurzanfrage über Bluetooth (Offline-Weg). Absender noch unbestätigt. */
export function HotspotSheet({ offer, theme, onAnswer }: {
  offer: drop.HotspotOffer; theme: Theme; onAnswer: (accept: boolean) => void;
}) {
  const s = styles(theme);
  return (
    <Sheet theme={theme} onClose={() => onAnswer(false)}>
      <View style={s.offer}>
        <View style={s.offerHead}>
          <Avatar name={offer.name} theme={theme} size={52} />
          <View style={s.grow}>
            <Text style={s.offerName} numberOfLines={1}>{offer.name}</Text>
            <Text style={s.meta}>{t('Name noch nicht bestätigt')}</Text>
          </View>
        </View>
        <Text style={s.offerText}>
          {t('{name} möchte dir {files} senden ({size}) – ohne WLAN, über eine direkte Verbindung.', {
            name: offer.name,
            files: filesLabel(offer.files),
            size: size(offer.total),
          })}
        </Text>
        <Text style={[s.meta, s.para]}>
          {t('Android fragt gleich, ob Snap sich kurz mit dem Netz „{ssid}“ verbinden darf.', { ssid: offer.ssid })}
        </Text>
        <View style={s.actions}>
          <Pressable onPress={() => onAnswer(false)} style={[s.tonal, s.grow]} accessibilityRole="button">
            <Text style={s.tonalText}>{t('Ablehnen')}</Text>
          </Pressable>
          <Pressable onPress={() => onAnswer(true)} style={[s.primary, s.grow]} accessibilityRole="button">
            <Text style={s.primaryText}>{t('Annehmen')}</Text>
          </Pressable>
        </View>
      </View>
    </Sheet>
  );
}

// ------------------------------------------------------------------ Einstellungen

export function DropPage({ theme, notify }: { theme: Theme; notify: (text: string) => void }) {
  const s = styles(theme);
  const [st, setSt] = useState<drop.DropStatus | null>(null);
  const [people, setPeople] = useState<drop.Contact[]>([]);
  const [blockedList, setBlocked] = useState<drop.Blocked[]>([]);
  const [now, setNow] = useState(Date.now() / 1000);
  // Bearbeiten: eigener Anzeigename (`'me'`) oder lokaler Name eines Kontakts (Schlüssel)
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState('');
  const [bg, setBg] = useState<drop.BackgroundState | null>(null);

  const load = useCallback(() => {
    drop.backgroundState().then(setBg).catch(() => {});
    drop.status().then(setSt).catch(() => {});
    drop.contacts().then(setPeople).catch(() => {});
    drop.blocked().then(setBlocked).catch(() => {});
  }, []);

  useEffect(() => {
    load();
    const off = core.onEvent((ev) => {
      if (ev.type === 'DropVisibilityChanged' || (ev.type === 'DropFinished' && ev.ok)) {
        load();
      }
    });
    const tick = setInterval(() => setNow(Date.now() / 1000), 15_000);
    return () => {
      off();
      clearInterval(tick);
    };
  }, [load]);

  const choose = async (mode: drop.Mode) => {
    try {
      setSt(await drop.setVisibility(mode));
      core.haptic('tick');
    } catch (e: any) {
      notify(String(e?.message ?? e));
    }
  };

  const changeBg = async (patch: Partial<drop.BackgroundState>) => {
    if (!bg) {
      return;
    }
    const next = { ...bg, ...patch };
    setBg(next);
    core.haptic('tick');
    await drop.setBackground(next.enabled, next.wlan, next.who);
    const fresh = await drop.backgroundState().catch(() => null);
    if (fresh) {
      setBg(fresh);
    }
    if (patch.enabled && fresh && !fresh.batteryExempt) {
      Alert.alert(
        t('Im Hintergrund empfangen'),
        t('Damit Snap den Empfang selbst starten darf, braucht es die Ausnahme von der Akku-Optimierung. Sie kostet keinen Akku – Snap läuft nur, wenn dich jemand weckt.'),
        [
          { text: t('Später'), style: 'cancel' },
          { text: t('Erlauben'), onPress: () => drop.requestBatteryExempt() },
        ],
      );
    }
  };

  const saveName = async (value: string | null) => {
    try {
      if (editing === 'me') {
        setSt(await drop.setName(value));
        drop.syncRadio().catch(() => {});
      } else if (editing) {
        await drop.renameContact(editing, value);
        load();
      }
      setEditing(null);
      core.haptic('tick');
    } catch (e: any) {
      notify(String(e?.message ?? e));
    }
  };
  const editor = (placeholder: string, reset: string | null) => (
    <View style={s.editRow}>
      <TextInput
        value={draft}
        onChangeText={setDraft}
        autoFocus
        maxLength={40}
        placeholder={placeholder}
        placeholderTextColor={theme.text3 as string}
        style={s.nameInput}
        onSubmitEditing={() => saveName(draft.trim() || null)}
        accessibilityLabel={t('Name')}
      />
      <Pressable onPress={() => saveName(draft.trim() || null)} style={s.tonal} accessibilityRole="button">
        <Text style={s.tonalText}>{t('Speichern')}</Text>
      </Pressable>
      {reset ? (
        <Pressable onPress={() => saveName(null)} style={s.iconBtn} accessibilityLabel={reset}>
          <Icon name="restore" size={20} color={theme.text2 as string} />
        </Pressable>
      ) : null}
    </View>
  );

  const modes: { key: drop.Mode; label: string }[] = [
    { key: 'off', label: t('Aus') },
    { key: 'contacts', label: t('Kontakte') },
    { key: 'everyone', label: t('Alle') },
  ];
  const minutes = st?.until ? Math.max(1, Math.ceil((st.until - now) / 60)) : 0;
  const explain = !st
    ? ''
    : st.mode === 'off'
      ? t('Niemand sieht dieses Gerät. Senden geht trotzdem.')
      : st.mode === 'contacts'
        ? t('Nur Kontakte sehen dich – wenn sie dich ebenfalls gespeichert haben.')
        : t('Alle in der Nähe sehen „{name}“ – noch {min} Min.', { name: st.name, min: minutes });

  return (
    <ScrollView contentContainerStyle={s.page}>
      <SectionLabel theme={theme} style={s.section}>{t('Sichtbarkeit')}</SectionLabel>
      <View style={s.card}>
        <View style={s.pad}>
          <View style={s.segment} accessibilityRole="radiogroup">
            {modes.map((x) => {
              const on = st?.mode === x.key;
              return (
                <Pressable key={x.key} onPress={() => choose(x.key)} style={[s.segItem, on && s.segOn]} accessibilityRole="radio" accessibilityState={{ selected: on }}>
                  <Text style={[s.segText, on && s.segTextOn]}>{x.label}</Text>
                </Pressable>
              );
            })}
          </View>
          <Text style={[s.meta, s.explain]}>{explain}</Text>
        </View>
        {st?.code ? (
          <>
            <View style={s.sep} />
            <Pressable onPress={() => Share.share({ message: st.code! }).catch(() => {})} style={s.row} android_ripple={{ color: theme.accentSoft as string }} accessibilityRole="button">
              <Icon name="link" size={22} color={theme.text2 as string} />
              <View style={s.grow}>
                <Text style={s.label}>{t('Code teilen')}</Text>
                <Text style={s.meta} numberOfLines={1}>{t('Für Geräte, die nicht in der Nähe sind')}</Text>
              </View>
              <Icon name="share" size={20} color={theme.text3 as string} />
            </Pressable>
          </>
        ) : null}
      </View>

      {bg?.supported ? (
        <>
          <SectionLabel theme={theme} style={s.section}>{t('Wenn Snap geschlossen ist')}</SectionLabel>
          <View style={s.card}>
            <View style={s.row}>
              <Icon name="nearby" size={22} color={theme.text2 as string} />
              <View style={s.grow}>
                <Text style={s.label}>{t('Im Hintergrund empfangen')}</Text>
                <Text style={s.meta}>{t('Kontakte wecken dein Gerät per Bluetooth – Snap läuft dabei nicht. Kaum Akkuverbrauch.')}</Text>
              </View>
              <Switch
                value={bg.enabled}
                onValueChange={(v) => changeBg({ enabled: v })}
                trackColor={{ true: theme.accent as string, false: theme.surface2 as string }}
                thumbColor={theme.surface as string}
                accessibilityLabel={t('Im Hintergrund empfangen')}
              />
            </View>
            {bg.enabled ? (
              <>
                <View style={s.sep} />
                <View style={s.row}>
                  <Icon name="wifi" size={22} color={theme.text2 as string} />
                  <View style={s.grow}>
                    <Text style={s.label}>{t('Auch über WLAN')}</Text>
                    <Text style={s.meta}>{t('Ständig empfangsbereit, auch für Geräte ohne Bluetooth. Braucht mehr Akku und zeigt eine dauerhafte Mitteilung.')}</Text>
                  </View>
                  <Switch
                    value={bg.wlan}
                    onValueChange={(v) => changeBg({ wlan: v })}
                    trackColor={{ true: theme.accent as string, false: theme.surface2 as string }}
                    thumbColor={theme.surface as string}
                    accessibilityLabel={t('Auch über WLAN')}
                  />
                </View>
                <View style={s.sep} />
                <View style={s.pad}>
                  <Text style={s.label}>{t('Wecken dürfen')}</Text>
                  <View style={[s.segment, s.explain]} accessibilityRole="radiogroup">
                    {([['contacts', t('Nur Kontakte')], ['visibility', t('Wie Sichtbarkeit')]] as const).map(([key, label]) => {
                      const on = bg.who === key;
                      return (
                        <Pressable key={key} onPress={() => changeBg({ who: key })} style={[s.segItem, on && s.segOn]} accessibilityRole="radio" accessibilityState={{ selected: on }}>
                          <Text style={[s.segText, on && s.segTextOn]}>{label}</Text>
                        </Pressable>
                      );
                    })}
                  </View>
                  <Text style={[s.meta, s.explain]}>
                    {bg.who === 'contacts'
                      ? t('Nur gespeicherte Kontakte können dein Gerät wecken.')
                      : t('Ist „Alle“ eingeschaltet, kann in diesen 10 Minuten jeder in der Nähe dein Gerät wecken; sonst nur Kontakte.')}
                  </Text>
                </View>
                {!bg.batteryExempt ? (
                  <>
                    <View style={s.sep} />
                    <Pressable onPress={() => drop.requestBatteryExempt()} style={s.row} android_ripple={{ color: theme.accentSoft as string }} accessibilityRole="button">
                      <Icon name="warning" size={22} color={theme.warn as string} />
                      <View style={s.grow}>
                        <Text style={s.label}>{t('Akku-Optimierung für Snap aufheben')}</Text>
                        <Text style={s.meta}>{t('Sonst darf Android den Empfang nicht selbst starten – du bekommst dann eine Mitteilung zum Antippen. Kostet selbst keinen Akku.')}</Text>
                      </View>
                    </Pressable>
                  </>
                ) : null}
              </>
            ) : null}
          </View>
        </>
      ) : null}

      <SectionLabel theme={theme} style={s.section}>{t('Dieses Gerät')}</SectionLabel>
      <View style={s.card}>
        {editing === 'me' ? (
          editor(st?.default_name ?? '', st?.custom_name ? t('Gerätename verwenden') : null)
        ) : (
          <Pressable
            onPress={() => { setDraft(st?.custom_name ? st.name : ''); setEditing('me'); }}
            style={s.row}
            android_ripple={{ color: theme.accentSoft as string }}
            accessibilityRole="button"
            accessibilityLabel={t('Name für andere ändern')}
          >
            <Icon name="phone" size={22} color={theme.text2 as string} />
            <View style={s.grow}>
              <Text style={s.label}>{st?.name ?? ''}</Text>
              <Text style={s.meta}>{t('Name für andere · Fingerabdruck {fp}', { fp: st?.fingerprint ?? '' })}</Text>
            </View>
            <Icon name="rename" size={20} color={theme.text3 as string} />
          </Pressable>
        )}
        <View style={s.sep} />
        <View style={s.row}>
          <Icon name="download" size={22} color={theme.text2 as string} />
          <View style={s.grow}>
            <Text style={s.label}>{t('Ablage')}</Text>
            <Text style={s.meta} numberOfLines={1}>{st?.dir.replace(/^\/storage\/emulated\/0\//, '') ?? ''}</Text>
          </View>
        </View>
      </View>

      <SectionLabel theme={theme} style={s.section}>{t('Kontakte')}</SectionLabel>
      <View style={s.card}>
        <View style={s.row}>
          <Icon name="globe" size={22} color={theme.text2 as string} />
          <View style={s.grow}>
            <Text style={s.label}>{t('Von überall empfangen')}</Text>
            <Text style={s.meta}>
              {t('Kontakte können dir auch senden, wenn ihr nicht in der Nähe seid – Ende-zu-Ende-verschlüsselt, Annehmen bleibt Pflicht. Je Kontakt abschaltbar.')}
            </Text>
          </View>
          <Switch
            value={!!st?.remote}
            onValueChange={(v) => drop.setRemote(v).then(setSt).catch((e) => notify(String(e?.message ?? e)))}
            trackColor={{ true: theme.accent as string, false: theme.surface2 as string }}
            thumbColor={theme.surface as string}
            accessibilityLabel={t('Von überall empfangen')}
          />
        </View>
        <View style={s.sep} />
        {people.length === 0 ? (
          <Text style={[s.meta, s.pad]}>{t('Nach einem Tausch kannst du das Gegenüber als Kontakt speichern. Kontakte erkennt Snap am Schlüssel, nicht am Namen.')}</Text>
        ) : (
          people.map((c, i) => (
            <View key={c.key}>
              {i > 0 ? <View style={s.sep} /> : null}
              {editing === c.key ? (
                editor(c.device_name, c.renamed ? t('Namen des Geräts verwenden') : null)
              ) : (
                <View style={s.row}>
                  <Avatar name={c.name} contact theme={theme} size={36} />
                  <Pressable
                    style={s.grow}
                    onPress={() => { setDraft(c.renamed ? c.name : ''); setEditing(c.key); }}
                    accessibilityRole="button"
                    accessibilityLabel={t('{name} umbenennen', { name: c.name })}
                  >
                    <Text style={s.label} numberOfLines={1}>{c.name}</Text>
                    <Text style={s.meta} numberOfLines={1}>{c.renamed ? `${c.device_name} · ${c.fingerprint}` : c.fingerprint}</Text>
                  </Pressable>
                  {st?.remote ? (
                    <Switch
                      value={c.remote_ok !== false}
                      onValueChange={(v) => drop.setContactRemote(c.key, v).then(load).catch((e) => notify(String(e?.message ?? e)))}
                      trackColor={{ true: theme.accent as string, false: theme.surface2 as string }}
                      thumbColor={theme.surface as string}
                      accessibilityLabel={t('{name} darf von überall senden', { name: c.name })}
                    />
                  ) : null}
                  <Pressable
                    onPress={() => drop.removeContact(c.key).then(() => { notify(t('Kontakt entfernt.')); load(); }).catch(() => {})}
                    style={s.iconBtn}
                    accessibilityLabel={t('{name} entfernen', { name: c.name })}
                  >
                    <Icon name="close" size={20} color={theme.text3 as string} />
                  </Pressable>
                </View>
              )}
            </View>
          ))
        )}
      </View>

      {blockedList.length > 0 ? (
        <>
          <SectionLabel theme={theme} style={s.section}>{t('Blockiert')}</SectionLabel>
          <View style={s.card}>
            {blockedList.map((b, i) => (
              <View key={b.key}>
                {i > 0 ? <View style={s.sep} /> : null}
                <View style={s.row}>
                  <Icon name="block" size={22} color={theme.text3 as string} />
                  <View style={s.grow}>
                    <Text style={s.label} numberOfLines={1}>{b.name}</Text>
                    <Text style={s.meta}>{t('seit {date}', { date: core.shortDate(b.blocked_at, locale()) })}</Text>
                  </View>
                  <Pressable onPress={() => drop.unblock(b.key).then(load).catch(() => {})} style={s.textBtnSmall} accessibilityRole="button">
                    <Text style={s.link}>{t('Freigeben')}</Text>
                  </Pressable>
                </View>
              </View>
            ))}
          </View>
        </>
      ) : null}

      <Text style={[s.meta, s.foot]}>
        {t('Übertragungen sind Ende-zu-Ende-verschlüsselt. Über Bluetooth gehen nur Verbindungsdaten, nie Dateien. Nichts wird ohne deine Zustimmung gespeichert.')}
      </Text>
    </ScrollView>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    fill: { flex: 1 },
    grow: { flex: 1, minWidth: 0 },
    center: { textAlign: 'center' },
    dim: { opacity: 0.45 },
    scrim: { flex: 1, backgroundColor: 'rgba(10,10,9,0.42)' },
    sheet: { paddingBottom: 20, paddingTop: 10, borderTopLeftRadius: 20, borderTopRightRadius: 20, backgroundColor: th.surface, maxHeight: '86%' },
    grabber: { alignSelf: 'center', width: 32, height: 4, borderRadius: 2, marginBottom: 6, opacity: 0.5, backgroundColor: th.text3 },
    sheetHead: { paddingHorizontal: 20, paddingTop: 8, paddingBottom: 8 },
    sheetTitle: { color: th.text, fontSize: 18, lineHeight: 24, fontWeight: '700' },
    list: { maxHeight: 420 },
    listInner: { paddingVertical: 4 },
    peer: { flexDirection: 'row', alignItems: 'center', gap: 14, minHeight: 68, paddingHorizontal: 20, paddingVertical: 8 },
    groupLabel: { paddingHorizontal: 20, paddingTop: 14, paddingBottom: 4 },
    rowInline: { flexDirection: 'row', alignItems: 'center', gap: 6 },
    label: { color: th.text, fontSize: 16, lineHeight: 22 },
    meta: { color: th.text2, fontSize: 13, lineHeight: 18 },
    accentText: { color: th.accent },
    dangerText: { color: th.danger },
    track: { height: 4, borderRadius: 2, backgroundColor: th.surface2, marginTop: 6, overflow: 'hidden' },
    bar: { height: 4, borderRadius: 2, backgroundColor: th.accent },
    empty: { alignItems: 'center', gap: 8, paddingHorizontal: 32, paddingVertical: 28 },
    emptyIcon: { width: 64, height: 64, borderRadius: 32, backgroundColor: th.surface2, justifyContent: 'center', alignItems: 'center', marginBottom: 4 },
    codeRow: { flexDirection: 'row', alignItems: 'center', gap: 10, paddingHorizontal: 20, paddingTop: 8 },
    input: { flex: 1, minHeight: 48, borderRadius: 24, paddingHorizontal: 16, backgroundColor: th.surface2, color: th.text, fontSize: 14 },
    textBtn: { alignSelf: 'center', minHeight: 48, justifyContent: 'center', paddingHorizontal: 16, marginTop: 4 },
    textBtnSmall: { minHeight: 40, justifyContent: 'center', paddingHorizontal: 12 },
    link: { color: th.accent, fontSize: 15, fontWeight: '700' },
    offer: { paddingHorizontal: 20, paddingTop: 6, gap: 12 },
    offerHead: { flexDirection: 'row', alignItems: 'center', gap: 14 },
    offerName: { color: th.text, fontSize: 18, lineHeight: 24, fontWeight: '700' },
    offerText: { color: th.text, fontSize: 15, lineHeight: 22 },
    preview: { width: 56, height: 56, borderRadius: 8, backgroundColor: th.surface2 },
    fileBox: { borderRadius: 12, backgroundColor: th.surface2, paddingHorizontal: 14, paddingVertical: 10, gap: 6 },
    fileRow: { flexDirection: 'row', alignItems: 'center', gap: 12 },
    fileName: { flex: 1, color: th.text, fontSize: 14, lineHeight: 20 },
    fileSize: { color: th.text2, fontSize: 13, fontVariant: ['tabular-nums'] },
    linkHost: { color: th.text, fontSize: 16, lineHeight: 22, fontWeight: '700' },
    nameInput: { flex: 1, minHeight: 44, borderRadius: 22, paddingHorizontal: 14, backgroundColor: th.surface2, color: th.text, fontSize: 15 },
    code: { color: th.text2, fontSize: 13, lineHeight: 18 },
    codeNum: { color: th.text, fontSize: 15, fontWeight: '700', letterSpacing: 1 },
    para: { marginTop: -4 },
    actions: { flexDirection: 'row', gap: 12, marginTop: 4 },
    primary: { minHeight: 48, borderRadius: 24, backgroundColor: th.accent, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 20 },
    primaryText: { color: th.onAccent, fontSize: 16, fontWeight: '700' },
    tonal: { minHeight: 48, borderRadius: 24, backgroundColor: th.surface2, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 20 },
    tonalText: { color: th.text, fontSize: 16, fontWeight: '600' },
    page: { paddingHorizontal: 16, paddingBottom: 40 },
    section: { marginTop: 20, marginBottom: 8, marginLeft: 4 },
    card: { borderRadius: 12, backgroundColor: th.surface, overflow: 'hidden' },
    pad: { padding: 16 },
    row: { flexDirection: 'row', alignItems: 'center', gap: 16, minHeight: 64, paddingHorizontal: 16, paddingVertical: 10 },
    sep: { height: StyleSheet.hairlineWidth, backgroundColor: th.line, marginLeft: 54 },
    iconBtn: { width: 44, height: 44, borderRadius: 22, justifyContent: 'center', alignItems: 'center' },
    segment: { flexDirection: 'row', padding: 4, borderRadius: 24, backgroundColor: th.surface2 },
    segItem: { flex: 1, minHeight: 40, borderRadius: 20, justifyContent: 'center', alignItems: 'center' },
    segOn: { backgroundColor: th.surface, elevation: 1 },
    segText: { color: th.text, fontSize: 15 },
    segTextOn: { fontWeight: '700' },
    explain: { marginTop: 12 },
    editRow: { flexDirection: 'row', alignItems: 'center', gap: 8, paddingHorizontal: 12, paddingVertical: 10 },
    foot: { marginTop: 20, marginHorizontal: 4, lineHeight: 19 },
  }));
