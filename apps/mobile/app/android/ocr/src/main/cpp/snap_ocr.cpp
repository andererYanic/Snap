// JNI-Brücke zu Tesseract: erkennt Text in einem Android-Bitmap (RGBA_8888).
// Eingaben kommen aus der App (Pixel aus Android dekodiert), Pfad zu den Sprachdaten aus dem
// privaten App-Ordner. Ergebnis: UTF-8-Bytes (höchstens MAX_TEXT); Kotlin dekodiert sie (kein
// NewStringUTF – das verlangt „modifiziertes“ UTF-8 und stürzt bei Emoji/abgeschnittenen Zeichen ab).
#include <android/bitmap.h>
#include <jni.h>
#include <tesseract/baseapi.h>

#include <memory>
#include <string>

namespace {
constexpr size_t MAX_TEXT = 1 << 20;

std::string str(JNIEnv* env, jstring s) {
  if (!s) return {};
  const char* c = env->GetStringUTFChars(s, nullptr);
  std::string out = c ? c : "";
  if (c) env->ReleaseStringUTFChars(s, c);
  return out;
}
}  // namespace

extern "C" JNIEXPORT jbyteArray JNICALL Java_de_snap_mobile_Ocr_nativeRecognize(
    JNIEnv* env, jclass, jobject bitmap, jstring dataPath, jstring langs) {
  AndroidBitmapInfo info;
  if (AndroidBitmap_getInfo(env, bitmap, &info) != ANDROID_BITMAP_RESULT_SUCCESS ||
      info.format != ANDROID_BITMAP_FORMAT_RGBA_8888 || info.width == 0 || info.height == 0 ||
      info.width > 10000 || info.height > 10000) {
    return nullptr;
  }
  auto api = std::make_unique<tesseract::TessBaseAPI>();
  const std::string path = str(env, dataPath);
  const std::string lang = str(env, langs);
  if (api->Init(path.c_str(), lang.c_str(), tesseract::OEM_LSTM_ONLY) != 0) {
    return nullptr;
  }
  api->SetPageSegMode(tesseract::PSM_AUTO);
  void* pixels = nullptr;
  if (AndroidBitmap_lockPixels(env, bitmap, &pixels) != ANDROID_BITMAP_RESULT_SUCCESS || !pixels) {
    return nullptr;
  }
  api->SetImage(static_cast<const unsigned char*>(pixels), static_cast<int>(info.width),
                static_cast<int>(info.height), 4, static_cast<int>(info.stride));
  std::unique_ptr<char[]> text(api->GetUTF8Text());
  AndroidBitmap_unlockPixels(env, bitmap);
  api->End();
  std::string out(text ? text.get() : "");
  if (out.size() > MAX_TEXT) out.resize(MAX_TEXT);
  jbyteArray arr = env->NewByteArray(static_cast<jsize>(out.size()));
  if (arr && !out.empty()) {
    env->SetByteArrayRegion(arr, 0, static_cast<jsize>(out.size()), reinterpret_cast<const jbyte*>(out.data()));
  }
  return arr;
}
