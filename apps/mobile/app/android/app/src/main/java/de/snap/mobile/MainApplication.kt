package de.snap.mobile

import android.app.Application
import com.facebook.react.PackageList
import com.facebook.react.ReactApplication
import com.facebook.react.ReactHost
import com.facebook.react.ReactNativeApplicationEntryPoint.loadReactNative
import com.facebook.react.defaults.DefaultReactHost.getDefaultReactHost

class MainApplication : Application(), ReactApplication {

  override val reactHost: ReactHost by lazy {
    getDefaultReactHost(
      context = applicationContext,
      packageList =
        PackageList(this).packages.apply {
          // Brücke zum eingebetteten Snap-Kern
          add(SnapPackage())
        },
    )
  }

  override fun onCreate() {
    super.onCreate()
    CoreHolder.installCrashHandler(this)
    loadReactNative(this)
    SyncWorker.schedule(this)
    Thread { Thumbnails.prune(this) }.start()
  }
}
