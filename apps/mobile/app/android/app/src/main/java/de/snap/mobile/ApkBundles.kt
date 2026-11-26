package de.snap.mobile

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageInstaller
import android.os.Build
import java.io.File
import java.util.zip.ZipFile

/**
 * App-Pakete aus mehreren APKs (Split-APKs): `.apks` (bundletool/SAI), `.xapk`, `.apkm`.
 * Die Datei ist ein ZIP; installiert wird über eine Sitzung des System-Installers, der wie bei
 * einer einzelnen APK selbst um Bestätigung bittet.
 *
 * Der Inhalt gilt als feindlich: nur Einträge auf `.apk`, höchstens [MAX_SPLITS] Stück und
 * [MAX_BYTES] insgesamt (gezählt beim Kopieren, nicht den Angaben im ZIP geglaubt); Namen der
 * Einträge werden für die Sitzung neu vergeben. Konfigurations-Splits für fremde Prozessoren
 * werden ausgelassen, Zusatzdaten (OBB) nicht übertragen.
 */
object ApkBundles {
  private const val MAX_SPLITS = 200
  private const val MAX_BYTES = 4L shl 30
  private const val ACTION = "de.snap.mobile.APK_INSTALL_RESULT"
  private val ABIS = setOf("armeabi", "armeabi_v7a", "arm64_v8a", "x86", "x86_64", "mips", "mips64", "riscv64")

  val BUNDLE = Regex("""\.(apks|xapk|apkm)$""", RegexOption.IGNORE_CASE)

  class Bundle(val apks: List<java.util.zip.ZipEntry>, val base: java.util.zip.ZipEntry, val obb: Boolean)

  /** Liest das Inhaltsverzeichnis und wählt die passenden APKs (null = kein App-Paket). */
  fun inspect(zip: ZipFile): Bundle? {
    val supported = Build.SUPPORTED_ABIS.map { it.replace('-', '_').lowercase() }.toSet()
    val apks = mutableListOf<java.util.zip.ZipEntry>()
    var obb = false
    var seen = 0
    for (e in zip.entries()) {
      if (++seen > 10_000) break
      if (e.isDirectory) continue
      val name = e.name.substringAfterLast('/').lowercase()
      if (name.endsWith(".obb")) obb = true
      if (!name.endsWith(".apk")) continue
      // config.arm64_v8a.apk, split_config.x86.apk, base-arm64_v8a.apk (bundletool) …
      val abi = ABIS.firstOrNull { a -> name.contains(".$a.") || name.contains("-$a.") || name.endsWith("_$a.apk") }
      if (abi != null && abi !in supported) continue
      apks += e
      if (apks.size > MAX_SPLITS) return null
    }
    if (apks.isEmpty()) return null
    val base = apks.firstOrNull { it.name.substringAfterLast('/').equals("base.apk", ignoreCase = true) }
      ?: apks.filterNot { val n = it.name.substringAfterLast('/').lowercase(); n.startsWith("config.") || n.startsWith("split_") }
        .maxByOrNull { it.size }
      ?: apks.maxByOrNull { it.size }!!
    return Bundle(apks, base, obb)
  }

  /** Haupt-APK in eine temporäre Datei kopieren (für die App-Infos); Aufrufer löscht sie. */
  fun extractBase(zip: ZipFile, b: Bundle, dir: File): File {
    dir.mkdirs()
    val out = File.createTempFile("base", ".apk", dir)
    zip.getInputStream(b.base).use { input -> out.outputStream().use { copyLimited(input, it, MAX_BYTES) } }
    return out
  }

  private fun copyLimited(input: java.io.InputStream, out: java.io.OutputStream, limit: Long): Long {
    val buf = ByteArray(256 * 1024)
    var total = 0L
    while (true) {
      val n = input.read(buf)
      if (n < 0) break
      total += n
      if (total > limit) throw IllegalStateException("App-Paket zu groß.")
      out.write(buf, 0, n)
    }
    return total
  }

  /**
   * Alle passenden APKs in eine Installer-Sitzung schreiben und abschicken. [done] wird genau
   * einmal mit dem Ergebnis aufgerufen: null = installiert, "aborted" = abgebrochen, sonst Meldung.
   */
  fun install(ctx: Context, file: File, done: (String?) -> Unit) {
    val installer = ctx.packageManager.packageInstaller
    var sessionId = -1
    try {
      ZipFile(file).use { zip ->
        val b = inspect(zip) ?: throw IllegalStateException("Keine App in diesem Paket.")
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        if (Build.VERSION.SDK_INT >= 31) {
          params.setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_REQUIRED)
        }
        sessionId = installer.createSession(params)
        installer.openSession(sessionId).use { session ->
          var budget = MAX_BYTES
          b.apks.forEachIndexed { i, e ->
            zip.getInputStream(e).use { input ->
              session.openWrite("split$i.apk", 0, -1).use { out ->
                budget -= copyLimited(input, out, budget)
                session.fsync(out)
              }
            }
          }
          val action = "$ACTION.$sessionId"
          val receiver = object : BroadcastReceiver() {
            override fun onReceive(c: Context, intent: Intent) {
              when (val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
                PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                  @Suppress("DEPRECATION")
                  val confirm = if (Build.VERSION.SDK_INT >= 33) {
                    intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                  } else {
                    intent.getParcelableExtra(Intent.EXTRA_INTENT)
                  }
                  if (confirm == null) {
                    finish(c, this, done, "Der Installer hat nicht geantwortet.")
                  } else {
                    c.startActivity(confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                  }
                }
                PackageInstaller.STATUS_SUCCESS -> finish(c, this, done, null)
                PackageInstaller.STATUS_FAILURE_ABORTED -> finish(c, this, done, "aborted")
                else -> finish(
                  c, this, done,
                  intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)?.take(300) ?: "Installation fehlgeschlagen ($status).",
                )
              }
            }
          }
          if (Build.VERSION.SDK_INT >= 33) {
            ctx.registerReceiver(receiver, IntentFilter(action), Context.RECEIVER_NOT_EXPORTED)
          } else {
            @Suppress("UnspecifiedRegisterReceiverFlag")
            ctx.registerReceiver(receiver, IntentFilter(action))
          }
          val flags = PendingIntent.FLAG_UPDATE_CURRENT or (if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0)
          val pi = PendingIntent.getBroadcast(ctx, sessionId, Intent(action).setPackage(ctx.packageName), flags)
          session.commit(pi.intentSender)
        }
      }
    } catch (e: Throwable) {
      if (sessionId >= 0) {
        try { installer.abandonSession(sessionId) } catch (_: Throwable) {}
      }
      done(e.message ?: e.toString())
    }
  }

  private fun finish(c: Context, r: BroadcastReceiver, done: (String?) -> Unit, result: String?) {
    try { c.unregisterReceiver(r) } catch (_: Throwable) {}
    done(result)
  }
}
