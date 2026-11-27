/** Gerät finden: lauter Wechselton für `secs` Sekunden (WebAudio, ohne Audiodatei).
 *  Gibt eine Funktion zurück, die den Ton sofort beendet. */
export function startAlarm(secs: number): () => void {
  let ctx: AudioContext | null = null;
  try {
    ctx = new AudioContext();
  } catch {
    return () => {};
  }
  const gain = ctx.createGain();
  gain.gain.value = 0.9;
  gain.connect(ctx.destination);
  const osc = ctx.createOscillator();
  osc.type = "square";
  osc.connect(gain);
  const t0 = ctx.currentTime;
  // 0,25 s hoch, 0,25 s tief, nach jeder Sekunde eine kurze Pause
  for (let s = 0; s < secs; s++) {
    osc.frequency.setValueAtTime(1046, t0 + s);
    osc.frequency.setValueAtTime(784, t0 + s + 0.25);
    gain.gain.setValueAtTime(0.9, t0 + s);
    gain.gain.setValueAtTime(0, t0 + s + 0.5);
  }
  osc.start(t0);
  osc.stop(t0 + secs);
  let done = false;
  const stop = () => {
    if (done) return;
    done = true;
    try {
      osc.stop();
    } catch {
      /* schon beendet */
    }
    ctx?.close().catch(() => {});
  };
  osc.onended = stop;
  return stop;
}
