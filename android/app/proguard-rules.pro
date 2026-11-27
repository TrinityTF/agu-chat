# The Rust side calls these by name through JNI.
-keep class lol.agu.chat.Core { *; }
-keep interface lol.agu.chat.CoreListener { *; }
-keep class * implements lol.agu.chat.CoreListener { *; }
