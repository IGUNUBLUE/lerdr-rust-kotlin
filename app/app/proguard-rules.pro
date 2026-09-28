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

# ML Kit barcode scanning — ComponentDiscovery instantiates the
# *Registrar classes named in manifest metadata reflectively; without
# their no-arg constructors the barcode scanner client is built with a
# null internal delegate and the first analysed frame NPEs inside
# CameraX's ImageAnalysisAbstractAnalyzer. Keep names + members for the
# mlkit surface and the firebase components it dispatches through.
-keep class com.google.mlkit.** { *; }
-keep class com.google.firebase.components.** { *; }
