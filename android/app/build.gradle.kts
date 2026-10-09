import java.util.Properties

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.compose.compiler)
}

// The Google OAuth client, written by scripts/build-android.sh (gitignored).
val secrets = Properties().apply {
    rootProject.file("secrets.properties").takeIf { it.exists() }?.inputStream()?.use(::load)
}

fun quoted(value: String) = "\"" + value.replace("\\", "\\\\").replace("\"", "\\\"") + "\""

android {
    namespace = "local.ytlite"
    compileSdk {
        version = release(37) { minorApiLevel = 1 }
    }

    defaultConfig {
        applicationId = "local.ytlite.android"
        // Must match MIN_SDK in scripts/build-android.sh.
        minSdk = 28
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        buildConfigField("String", "GOOGLE_CLIENT_ID", quoted(secrets.getProperty("google.clientId", "")))
        buildConfigField("String", "GOOGLE_CLIENT_SECRET", quoted(secrets.getProperty("google.clientSecret", "")))
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            // A personal sideloaded app: sign release builds with the debug key.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.core.splashscreen)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.browser)
    implementation(libs.androidx.datastore.preferences)

    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.tooling.preview)
    implementation(libs.compose.material3)
    implementation(libs.compose.material3.navigation.suite)
    implementation(libs.compose.material.icons.extended)
    debugImplementation(libs.compose.ui.tooling)

    implementation(libs.media3.exoplayer)
    implementation(libs.media3.exoplayer.hls)
    implementation(libs.media3.session)
    implementation(libs.media3.ui)

    implementation(libs.coil.compose)
    implementation(libs.coil.network)
    implementation(libs.kotlinx.coroutines.guava)

    // The Rust core's UniFFI bindings call it through JNA.
    implementation(libs.jna) { artifact { type = "aar" } }

    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.junit)
}
