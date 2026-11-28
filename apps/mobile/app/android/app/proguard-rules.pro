# R8 (Release): Code schrumpfen, ohne Laufzeit-Reflexion zu brechen.

# Snap-Kern über UniFFI: die Kotlin-Anbindung ruft die Rust-Bibliothek über JNA auf (Reflexion,
# Strukturen und Rückrufe werden zur Laufzeit gesucht).
-keep class uniffi.** { *; }
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.** { public *; }
-dontwarn java.awt.**
-dontwarn com.sun.jna.**

# Eigene Klassen: React-Native-Module, Dienste, Empfänger und JNI (Texterkennung) – klein, daher
# vollständig behalten statt einzeln aufzuzählen.
-keep class de.snap.mobile.** { *; }
-keepclasseswithmembernames class * { native <methods>; }

# Hermes/React Native bringen eigene Regeln mit (consumer rules); Warnungen aus optionalen
# Abhängigkeiten nicht als Fehler werten.
-dontwarn com.facebook.**

# WorkManager nutzt Room: die erzeugte Datenbank-Klasse wird per Reflexion über ihren leeren
# Konstruktor angelegt (Emulator-Test: NoSuchMethodException WorkDatabase_Impl.<init>).
-keep class * extends androidx.room.RoomDatabase { <init>(); }
-keep class androidx.work.impl.WorkDatabase_Impl { *; }
-keep class androidx.work.** { *; }
