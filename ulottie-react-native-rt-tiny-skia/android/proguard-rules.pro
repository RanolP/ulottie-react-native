# The JNI adapter resolves these classes/methods by name (RegisterNatives,
# GetMethodID); renaming or stripping them breaks the native bridge.
-keep class dev.ulottie.rttinyskia.UlottieRtNative { *; }
-keep class dev.ulottie.rttinyskia.UlottieRtView { *; }
