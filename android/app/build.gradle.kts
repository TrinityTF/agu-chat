import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
}

// The app version follows the Rust crates (native/android/Cargo.toml): one version for every platform.
val cargoVersion: String = file("../../native/android/Cargo.toml").readLines()
    .first { it.startsWith("version") }.substringAfter('"').substringBefore('"')
val versionParts = cargoVersion.substringBefore('-').split('.').map { it.toInt() }

// Release signing comes from android/keystore.properties (never committed) or the CI's environment.
val keyProps = Properties().apply {
    val f = rootProject.file("keystore.properties")
    if (f.exists()) f.inputStream().use { load(it) }
}
fun signingValue(key: String, env: String): String? = keyProps.getProperty(key) ?: System.getenv(env)

android {
    namespace = "lol.agu.chat"
    compileSdk = 37
    compileSdkMinor = 1

    defaultConfig {
        applicationId = "lol.agu.chat"
        minSdk = 28
        targetSdk = 36
        versionName = cargoVersion
        versionCode = versionParts[0] * 1_000_000 + versionParts[1] * 1_000 + versionParts[2]
    }

    signingConfigs {
        create("release") {
            val store = signingValue("storeFile", "AGU_ANDROID_KEYSTORE")
            if (store != null) {
                storeFile = file(store)
                storePassword = signingValue("storePassword", "AGU_ANDROID_KEYSTORE_PASSWORD")
                keyAlias = signingValue("keyAlias", "AGU_ANDROID_KEY_ALIAS")
                keyPassword = signingValue("keyPassword", "AGU_ANDROID_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            if (signingValue("storeFile", "AGU_ANDROID_KEYSTORE") != null) signingConfig = signingConfigs.getByName("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures { compose = true }

    packaging { jniLibs { useLegacyPackaging = false } }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2026.11.01")
    implementation(composeBom)
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")

    implementation("androidx.core:core-ktx:1.19.1")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.11.1")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.1")

    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.13.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")

    implementation("io.coil-kt.coil3:coil-compose:3.6.3")
    implementation("io.coil-kt.coil3:coil-network-okhttp:3.6.3")
    implementation("io.coil-kt.coil3:coil-gif:3.6.3")
}
