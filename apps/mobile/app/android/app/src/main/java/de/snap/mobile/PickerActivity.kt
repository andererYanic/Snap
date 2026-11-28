package de.snap.mobile

import android.content.ClipData
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import com.facebook.react.ReactActivity
import com.facebook.react.ReactActivityDelegate
import com.facebook.react.defaults.DefaultNewArchitectureEntryPoint.fabricEnabled
import com.facebook.react.defaults.DefaultReactActivityDelegate

/**
 * Snap als Dateiauswahl für andere Apps (`ACTION_GET_CONTENT`).
 *
 * Sicherheit (docs/SECURITY_MODEL.md):
 * - Herausgegeben wird nur, was der Mensch hier antippt: jede Datei wird vorher vom Kern geprüft
 *   (`checkSharedPath`: im gemeinsamen Speicher, nie Snaps eigene Ordner, keine Symlinks
 *   dorthin) und als `content://`-Adresse nur lesend und nur für diesen Aufruf freigegeben.
 * - Verlangt die App bestimmte Arten (z. B. nur Bilder), wird nur Passendes angenommen.
 * - Keine Dateien anderer Geräte; nichts wird geschrieben.
 * - Gegen Überlagerung durch andere Apps (Tapjacking): Berührungen werden ignoriert, solange ein
 *   fremdes Fenster darüber liegt.
 */
class PickerActivity : ReactActivity() {
  /** Gewünschte Arten (leer = alle) und ob mehrere Dateien gewählt werden dürfen. */
  val mimes: List<String> by lazy {
    val extra = intent.getStringArrayExtra(Intent.EXTRA_MIME_TYPES)?.toList().orEmpty()
    (extra + listOfNotNull(intent.type)).map { it.lowercase().take(100) }.filter { it.isNotBlank() && it != "*/*" }.distinct()
  }
  val multiple: Boolean by lazy { intent.getBooleanExtra(Intent.EXTRA_ALLOW_MULTIPLE, false) }

  override fun getMainComponentName(): String = "SnapPicker"

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    window.decorView.filterTouchesWhenObscured = true
  }

  /** Passt die Art einer Datei zu den gewünschten Arten? */
  fun accepts(mime: String): Boolean =
    mimes.isEmpty() || mimes.any { want ->
      if (want.endsWith("/*")) mime.startsWith(want.dropLast(1)) else mime == want
    }

  /** Ergebnis an die aufrufende App: Adressen nur lesend, nur für diesen Aufruf. */
  fun deliver(uris: List<Uri>) {
    val result = Intent()
    result.data = uris.first()
    if (uris.size > 1 || multiple) {
      val clip = ClipData.newRawUri("", uris.first())
      uris.drop(1).forEach { clip.addItem(ClipData.Item(it)) }
      result.clipData = clip
    }
    result.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    setResult(RESULT_OK, result)
    finish()
  }

  fun cancel() {
    setResult(RESULT_CANCELED)
    finish()
  }

  override fun createReactActivityDelegate(): ReactActivityDelegate =
    DefaultReactActivityDelegate(this, mainComponentName, fabricEnabled)
}
