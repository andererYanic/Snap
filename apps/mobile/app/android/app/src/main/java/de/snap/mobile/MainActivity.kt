package de.snap.mobile

import android.content.Intent
import android.os.Bundle
import com.facebook.react.ReactActivity
import com.facebook.react.ReactActivityDelegate
import com.facebook.react.defaults.DefaultNewArchitectureEntryPoint.fabricEnabled
import com.facebook.react.defaults.DefaultReactActivityDelegate

class MainActivity : ReactActivity() {
  companion object {
    /** App im Vordergrund: Angebote zeigt dann die Oberfläche statt einer Mitteilung. */
    @Volatile var visible = false
  }

  override fun onResume() {
    super.onResume()
    visible = true
  }

  override fun onPause() {
    visible = false
    super.onPause()
  }

  /**
   * Returns the name of the main component registered from JavaScript. This is used to schedule
   * rendering of the component.
   */
  override fun getMainComponentName(): String = "SnapMobile"

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    // Aus dem „Teilen“-Menü einer anderen App gestartet → für Snap Drop vormerken
    if (savedInstanceState == null) {
      ShareInbox.accept(applicationContext, intent)
      // „Öffnen mit Snap“ bzw. „Ordner öffnen“ aus einer anderen App
      ExternalOpen.accept(applicationContext, intent)
    }
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    ShareInbox.accept(applicationContext, intent)
    ExternalOpen.accept(applicationContext, intent)
  }

  /**
   * Returns the instance of the [ReactActivityDelegate]. We use [DefaultReactActivityDelegate]
   * which allows you to enable New Architecture with a single boolean flags [fabricEnabled]
   */
  override fun createReactActivityDelegate(): ReactActivityDelegate =
      DefaultReactActivityDelegate(this, mainComponentName, fabricEnabled)
}
