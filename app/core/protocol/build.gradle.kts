plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

dependencies {
    api(project(":core:model"))
    api(libs.kotlinx.serialization.json)
    // `frame_zstd` — compileOnly so the jar never lands on an Android
    // packaging classpath; :app carries the @aar variant for the device
    // and testImplementation jars for JVM unit tests.
    compileOnly(libs.zstd.jni)
    testImplementation(libs.zstd.jni)

    testImplementation(libs.junit)
    testImplementation(libs.truth)
    testImplementation(project(":core:testing"))
}
