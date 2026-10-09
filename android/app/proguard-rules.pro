# UniFFI bindings (uniffi/yt_lite_ffi) call into the Rust core through JNA,
# which finds native methods and structures by reflection.
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-keep class uniffi.yt_lite_ffi.** { *; }
-dontwarn java.awt.**
