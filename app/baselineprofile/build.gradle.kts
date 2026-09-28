plugins {
    id("com.android.test")
}

android {
    namespace = "com.lerdr.baselineprofile"
    compileSdk = 37
    targetProjectPath = ":app"

    // BaselineProfileRule force-stops the target app on every iteration;
    // self-instrumenting keeps the runner in this module's own process and
    // packages the full runtime classpath into the test APK.
    experimentalProperties["android.experimental.self-instrumenting"] = true

    defaultConfig {
        minSdk = 28
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(libs.kotlin.stdlib)
    implementation(libs.androidx.benchmark.macro.junit4)
    implementation(libs.androidx.test.ext.junit)
    implementation(libs.androidx.test.uiautomator)
}
