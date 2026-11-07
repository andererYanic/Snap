package de.snap.mobile

import android.content.Context
import android.graphics.Typeface
import com.facebook.react.common.assets.ReactFontManager
import java.io.File
import java.io.InputStream

/**
 * Schriftart der App. Standard ist Inconsolata (SIL Open Font License, liegt der App bei). Eine
 * eigene TTF/OTF-Datei kann gewählt werden: Sie wird geprüft (Größe, Dateikopf, von Android
 * ladbar) und in den privaten Speicher der App kopiert. Registriert wird unter dem Namen
 * [FAMILY], den alle Texte der App verwenden.
 */
object Fonts {
  const val FAMILY = "SnapFont"
  private const val MAX_BYTES = 15L * 1024 * 1024

  private fun dir(context: Context) = File(context.filesDir, "fonts").apply { mkdirs() }
  private fun custom(context: Context) = File(dir(context), "eigene.ttf")
  private fun nameFile(context: Context) = File(dir(context), "eigene.name")

  /** Aktuelle Schrift registrieren (beim Start und nach jeder Änderung). */
  fun apply(context: Context) {
    val fm = ReactFontManager.getInstance()
    val own = custom(context)
    val regular: Typeface
    val bold: Typeface
    if (own.isFile) {
      regular = try { Typeface.createFromFile(own) } catch (_: Throwable) { own.delete(); return apply(context) }
      bold = Typeface.create(regular, Typeface.BOLD)
    } else {
      regular = Typeface.createFromAsset(context.assets, "fonts/Inconsolata-Regular.ttf")
      bold = Typeface.createFromAsset(context.assets, "fonts/Inconsolata-Bold.ttf")
    }
    fm.setTypeface(FAMILY, Typeface.NORMAL, regular)
    fm.setTypeface(FAMILY, Typeface.BOLD, bold)
    fm.setTypeface(FAMILY, Typeface.ITALIC, Typeface.create(regular, Typeface.ITALIC))
    fm.setTypeface(FAMILY, Typeface.BOLD_ITALIC, Typeface.create(bold, Typeface.BOLD_ITALIC))
  }

  /** Name der eigenen Schrift oder null (= Inconsolata). */
  fun customName(context: Context): String? =
    if (custom(context).isFile) nameFile(context).takeIf { it.isFile }?.readText()?.take(120) ?: "Eigene Schrift" else null

  /**
   * Übernimmt eine Schriftdatei. Prüft Größe und Dateikopf (TrueType/OpenType) und ob Android sie
   * laden kann – erst dann ersetzt sie die bisherige. Fehler → Ausnahme mit Text für den Nutzer.
   */
  fun import(context: Context, input: InputStream, displayName: String) {
    val tmp = File(dir(context), "neu.tmp")
    try {
      var total = 0L
      tmp.outputStream().use { out ->
        val buf = ByteArray(64 * 1024)
        while (true) {
          val n = input.read(buf)
          if (n < 0) break
          total += n
          if (total > MAX_BYTES) throw IllegalArgumentException("Die Schriftdatei ist zu groß (höchstens 15 MB).")
          out.write(buf, 0, n)
        }
      }
      val head = ByteArray(4)
      tmp.inputStream().use { if (it.read(head) != 4) throw IllegalArgumentException("Keine Schriftdatei.") }
      val ok = (head[0].toInt() == 0 && head[1].toInt() == 1 && head[2].toInt() == 0 && head[3].toInt() == 0) ||
        String(head, Charsets.ISO_8859_1) in setOf("true", "OTTO")
      if (!ok) throw IllegalArgumentException("Keine TrueType- oder OpenType-Schrift (TTF/OTF).")
      // Lässt Android die Datei zu? (wirft bei kaputten Schriften)
      Typeface.createFromFile(tmp)
      if (!tmp.renameTo(custom(context))) throw IllegalStateException("Schrift ließ sich nicht speichern.")
      nameFile(context).writeText(displayName.replace(Regex("\\.(ttf|otf)$", RegexOption.IGNORE_CASE), "").take(120))
      apply(context)
    } catch (e: IllegalArgumentException) {
      throw e
    } catch (e: Throwable) {
      throw IllegalArgumentException("Diese Schrift lässt sich nicht verwenden.")
    } finally {
      tmp.delete()
    }
  }

  /** Zurück zu Inconsolata. */
  fun reset(context: Context) {
    custom(context).delete()
    nameFile(context).delete()
    apply(context)
  }
}
