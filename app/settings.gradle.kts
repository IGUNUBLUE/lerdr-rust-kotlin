rootProject.name = "lerdr"

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

include(":core:model")
include(":core:protocol")
include(":core:e2ee")
include(":core:terminal")
include(":core:testing")
include(":core:transport")
include(":core:store")
include(":core:conversation")
include(":core:designsystem")
include(":core:data")
include(":navigation")
include(":app")
