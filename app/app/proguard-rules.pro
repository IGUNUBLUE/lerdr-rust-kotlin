# kotlinx.serialization — keep the serializer lookup path used by
# `serializer()`/`Json` entry points on @Serializable models.
-keepattributes *Annotation*, InnerClasses
-dontnote kotlinx.serialization.AnnotationsKt
-keepclassmembers class kotlinx.serialization.json.** {
    *** Companion;
}
-keepclasseswithmembers class kotlinx.serialization.json.** {
    kotlinx.serialization.KSerializer serializer(...);
}
-keep,includedescriptorclasses class com.lerdr.**$$serializer { *; }
-dontnote com.lerdr.**$serializer

# zstd-jni — `frame_zstd` native bindings.
-keepclasseswithmembernames class com.github.luben.zstd.** {
    native <methods>;
}
