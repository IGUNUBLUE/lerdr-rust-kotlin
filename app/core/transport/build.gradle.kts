plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

dependencies {
    api(project(":core:protocol"))
    api(project(":core:e2ee"))
    api(libs.kotlinx.coroutines.core)
    implementation(libs.okhttp)

    testImplementation(libs.junit)
    testImplementation(libs.truth)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.turbine)
    testImplementation(libs.mockwebserver3)
    // The wire probe exercises `frame_zstd` decompression live.
    testImplementation(libs.zstd.jni)
    testImplementation(project(":core:testing"))
}
