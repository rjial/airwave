import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// The OpenSL ES output is native code. It is built only when the Android NDK
// and CMake are installed (Android Studio > SDK Manager > SDK Tools);
// without them the app still builds and uses AudioTrack.
val sdkDir: File? = run {
    val props = Properties()
    rootProject.file("local.properties").takeIf { it.exists() }?.inputStream()?.use { props.load(it) }
    (props.getProperty("sdk.dir") ?: System.getenv("ANDROID_HOME") ?: System.getenv("ANDROID_SDK_ROOT"))?.let { File(it) }
}
val hasNativeTools = sdkDir != null &&
    (File(sdkDir, "ndk").isDirectory || File(sdkDir, "ndk-bundle").isDirectory) &&
    File(sdkDir, "cmake").isDirectory

android {
    namespace = "com.airwave.client"
    compileSdk = 35
    buildToolsVersion = "36.1.0"

    defaultConfig {
        applicationId = "com.airwave.client"
        minSdk = 26
        targetSdk = 35
        versionCode = 2
        versionName = "0.1.0"
        externalNativeBuild {
            cmake {
                // Oboe's prefab package ships against the shared C++ runtime.
                arguments += "-DANDROID_STL=c++_shared"
            }
        }
    }

    // Release signing is configured only when the keystore env vars are present
    // (set in CI from repo secrets). Without them the release build is unsigned,
    // so local builds keep working.
    val releaseKeystore: File? = System.getenv("ANDROID_KEYSTORE_FILE")?.let { File(it) }?.takeIf { it.exists() }
    signingConfigs {
        if (releaseKeystore != null) {
            create("release") {
                storeFile = releaseKeystore
                storePassword = System.getenv("ANDROID_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("ANDROID_KEY_ALIAS")
                keyPassword = System.getenv("ANDROID_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            if (releaseKeystore != null) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    buildFeatures {
        compose = true
        prefab = true
    }
    if (hasNativeTools) {
        externalNativeBuild {
            cmake {
                path = file("src/main/cpp/CMakeLists.txt")
            }
        }
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2024.12.01")
    implementation(composeBom)
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.8.7")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    // Oboe output backend (native); prefab exposes it to CMake.
    implementation("com.google.oboe:oboe:1.9.3")
}
