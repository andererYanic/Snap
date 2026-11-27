package de.snap.mobile

import android.app.Activity
import android.app.AlertDialog
import android.os.Build
import android.os.Bundle
import android.view.WindowManager
import java.lang.ref.WeakReference

/**
 * Snap-Drop-Angebot als Fenster über der gerade genutzten App bzw. dem Sperrbildschirm
 * (Einstellung „Anfragen als Popup zeigen“). Nur von der eigenen App startbar; die Antwort
 * geht wie bei der Mitteilung an [DropNotify.respond].
 */
class DropOfferActivity : Activity() {
  var offerId = -1L
    private set
  private var dialog: AlertDialog? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    if (Build.VERSION.SDK_INT >= 27) {
      setShowWhenLocked(true)
      setTurnScreenOn(true)
    } else {
      @Suppress("DEPRECATION")
      window.addFlags(WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON)
    }
    show()
  }

  override fun onNewIntent(intent: android.content.Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    dialog?.setOnDismissListener(null)
    dialog?.dismiss()
    show()
  }

  private fun show() {
    offerId = intent.getLongExtra("id", -1)
    if (offerId < 0) return finish()
    DropNotify.popup = WeakReference(this)
    val name = intent.getStringExtra("name")?.take(64) ?: getString(R.string.drop_share)
    val text = intent.getStringExtra("text")?.take(600) ?: ""
    var answered = false
    dialog = AlertDialog.Builder(this, android.R.style.Theme_DeviceDefault_Dialog_Alert)
      .setTitle(getString(R.string.drop_popup_title, name))
      .setMessage(text)
      .setPositiveButton(R.string.drop_accept) { _, _ ->
        answered = true
        DropNotify.respond(this, offerId, true)
      }
      .setNegativeButton(R.string.drop_decline) { _, _ ->
        answered = true
        DropNotify.respond(this, offerId, false)
      }
      .setOnDismissListener {
        // Wegtippen heißt: später entscheiden (die Mitteilung bleibt)
        if (!answered) android.util.Log.i("SnapDrop", "Popup ohne Antwort geschlossen")
        finish()
      }
      .create()
    dialog?.show()
  }

  override fun onDestroy() {
    if (DropNotify.popup?.get() === this) DropNotify.popup = null
    dialog?.setOnDismissListener(null)
    dialog?.dismiss()
    super.onDestroy()
  }
}
