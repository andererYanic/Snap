# Texterkennung (OCR): Leptonica und Tesseract 5 aus den Quellen, fest auf Commit-Hashes.
# Gebaut als statische Bibliotheken ohne Bild-, Netz- und Archivbibliotheken (die Pixel kommen
# fertig aus Android), dazu eine kleine JNI-Brücke libsnap_ocr.so.
cmake_minimum_required(VERSION 3.22)
project(snap_ocr C CXX)
include(ExternalProject)

set(LEPTONICA_COMMIT 13275a278eb55b5746e33f95fbf5a2c8f604b3ab)  # 1.87.0
set(TESSERACT_COMMIT 4292f6890aa86ef64eba32090358f40f69dd6d28)  # 5.5.5
set(CPU_FEATURES_COMMIT ba4bffa86cbb5456bdb34426ad22b9551278e2c0)  # v0.9.0 (von Tesseract unter Android verlangt)

set(DEPS ${CMAKE_BINARY_DIR}/ocr-deps)
set(COMMON_ARGS
  -DCMAKE_TOOLCHAIN_FILE=${CMAKE_TOOLCHAIN_FILE}
  -DANDROID_ABI=${ANDROID_ABI}
  -DANDROID_PLATFORM=${ANDROID_PLATFORM}
  -DANDROID_STL=c++_shared
  -DCMAKE_BUILD_TYPE=Release
  -DCMAKE_INSTALL_PREFIX=${DEPS}
  -DCMAKE_PREFIX_PATH=${DEPS}
  -DCMAKE_FIND_ROOT_PATH=${DEPS}
  -DBUILD_SHARED_LIBS=OFF
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON
  -DCMAKE_POLICY_VERSION_MINIMUM=3.5
  # Nie Bibliotheken des Bau-Rechners finden (pkg-config lieferte sonst /usr/include)
  -DCMAKE_DISABLE_FIND_PACKAGE_PkgConfig=ON)

ExternalProject_Add(leptonica
  GIT_REPOSITORY https://github.com/DanBloomberg/leptonica.git
  GIT_TAG ${LEPTONICA_COMMIT}
  GIT_SHALLOW OFF
  UPDATE_DISCONNECTED ON
  CMAKE_ARGS ${COMMON_ARGS}
    -DSW_BUILD=OFF -DBUILD_PROG=OFF
    -DENABLE_ZLIB=OFF -DENABLE_PNG=OFF -DENABLE_GIF=OFF -DENABLE_JPEG=OFF
    -DENABLE_TIFF=OFF -DENABLE_WEBP=OFF -DENABLE_OPENJPEG=OFF
  BUILD_BYPRODUCTS ${DEPS}/lib/libleptonica.a)

ExternalProject_Add(cpu_features
  GIT_REPOSITORY https://github.com/google/cpu_features.git
  GIT_TAG ${CPU_FEATURES_COMMIT}
  GIT_SHALLOW OFF
  UPDATE_DISCONNECTED ON
  CMAKE_ARGS ${COMMON_ARGS} -DBUILD_TESTING=OFF -DBUILD_EXECUTABLE=OFF -DENABLE_INSTALL=ON
  BUILD_BYPRODUCTS ${DEPS}/lib/libcpu_features.a ${DEPS}/lib/libndk_compat.a)

ExternalProject_Add(tesseract
  DEPENDS leptonica cpu_features
  GIT_REPOSITORY https://github.com/tesseract-ocr/tesseract.git
  GIT_TAG ${TESSERACT_COMMIT}
  GIT_SHALLOW OFF
  UPDATE_DISCONNECTED ON
  CMAKE_ARGS ${COMMON_ARGS}
    -DLeptonica_DIR=${DEPS}/lib/cmake/leptonica
    -DCpuFeaturesNdkCompat_DIR=${DEPS}/lib/cmake/CpuFeaturesNdkCompat
    -DCpuFeatures_DIR=${DEPS}/lib/cmake/CpuFeatures
    -DSW_BUILD=OFF -DBUILD_TRAINING_TOOLS=OFF -DBUILD_TESTS=OFF
    -DGRAPHICS_DISABLED=ON -DDISABLED_LEGACY_ENGINE=ON -DOPENMP_BUILD=OFF
    -DDISABLE_ARCHIVE=ON -DDISABLE_CURL=ON -DENABLE_LTO=OFF -DINSTALL_CONFIGS=OFF
    -DDISABLE_TIFF=ON
  BUILD_BYPRODUCTS ${DEPS}/lib/libtesseract.a)

add_library(snap_ocr SHARED snap_ocr.cpp)
add_dependencies(snap_ocr tesseract)
target_include_directories(snap_ocr PRIVATE ${DEPS}/include)
target_compile_options(snap_ocr PRIVATE -Wall -O2 -fvisibility=hidden)
# Nur die eigene JNI-Schnittstelle exportieren, ungenutzten Code entfernen
target_link_options(snap_ocr PRIVATE -Wl,--gc-sections -Wl,--exclude-libs,ALL)
target_link_libraries(snap_ocr PRIVATE
  ${DEPS}/lib/libtesseract.a ${DEPS}/lib/libleptonica.a
  ${DEPS}/lib/libndk_compat.a ${DEPS}/lib/libcpu_features.a jnigraphics log)
