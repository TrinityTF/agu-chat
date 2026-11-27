//! `lol.agu.chat.Core`'s native methods. Kotlin passes a `CoreListener` that hears every event and every call's
//! answer as JSON strings:
//!
//! ```kotlin
//! interface CoreListener {
//!     fun onEvent(kind: String, json: String)
//!     fun onReply(id: Long, ok: Boolean, json: String)
//!     fun openUrl(url: String)
//! }
//! ```
//!
//! Callbacks come on Rust threads (attached to the JVM once, for good); the Kotlin side hops to its own scope.
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use jni::objects::{GlobalRef, JByteArray, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jlong};
use jni::{JNIEnv, JavaVM};
use serde_json::Value;

use crate::session::Session;

struct Glue {
    vm: JavaVM,
    listener: GlobalRef,
    session: OnceLock<Session>,
}

static GLUE: OnceLock<Arc<Glue>> = OnceLock::new();

impl Glue {
    /// Run `f` with a JNIEnv of this thread (attached permanently the first time) inside a local frame.
    fn with_env(&self, f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<()>) {
        let Ok(mut env) = self.vm.attach_current_thread_permanently() else { return };
        let listener = self.listener.as_obj();
        let _ = env.with_local_frame(16, |env| f(env, listener));
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
    }

    fn event(&self, kind: &str, payload: &Value) {
        let json = payload.to_string();
        self.with_env(|env, l| {
            let k = env.new_string(kind)?;
            let j = env.new_string(&json)?;
            env.call_method(l, "onEvent", "(Ljava/lang/String;Ljava/lang/String;)V", &[JValue::Object(&k), JValue::Object(&j)])?;
            Ok(())
        });
    }

    fn reply(&self, id: i64, res: Result<Value, String>) {
        let (ok, json) = match res {
            Ok(v) => (true, v.to_string()),
            Err(e) => (false, Value::String(e).to_string()),
        };
        self.with_env(|env, l| {
            let j = env.new_string(&json)?;
            env.call_method(l, "onReply", "(JZLjava/lang/String;)V", &[JValue::Long(id), JValue::Bool(ok as jboolean), JValue::Object(&j)])?;
            Ok(())
        });
    }

    fn open_url(&self, url: &str) {
        self.with_env(|env, l| {
            let u = env.new_string(url)?;
            env.call_method(l, "openUrl", "(Ljava/lang/String;)V", &[JValue::Object(&u)])?;
            Ok(())
        });
    }
}

fn jstr(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s).map(|s| s.into()).unwrap_or_default()
}

/// `Core.nativeStart(dataDir, key, version, listener)`: start the backend once. `key` is the 32-byte secrets key the
/// app keeps wrapped by the Android Keystore.
#[no_mangle]
pub extern "system" fn Java_lol_agu_chat_Core_nativeStart(mut env: JNIEnv, _c: JClass, data_dir: JString, key: JByteArray, version: JString, listener: JObject) {
    if GLUE.get().is_some() {
        return;
    }
    let dir = PathBuf::from(jstr(&mut env, &data_dir));
    let version = jstr(&mut env, &version);
    let key = env.convert_byte_array(&key).unwrap_or_default();
    let (Ok(vm), Ok(listener)) = (env.get_java_vm(), env.new_global_ref(listener)) else { return };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
        if let Ok(k) = <[u8; 32]>::try_from(key.as_slice()) {
            agu_chat_core::safe_storage::set_platform_key(k);
        }
        let glue = Arc::new(Glue { vm, listener, session: OnceLock::new() });
        let _ = GLUE.set(glue.clone());
        let g = glue.clone();
        agu_chat_core::backend::set_url_opener(Arc::new(move |url: &str| g.open_url(url)));
        let g = glue.clone();
        let session = Session::start(dir, version, Arc::new(move |kind: &str, v: &Value| g.event(kind, v)));
        let _ = glue.session.set(session);
    }));
}

/// `Core.nativeCall(id, method, argsJson)`: the answer comes back through `listener.onReply(id, ok, json)`.
#[no_mangle]
pub extern "system" fn Java_lol_agu_chat_Core_nativeCall(mut env: JNIEnv, _c: JClass, id: jlong, method: JString, args: JString) {
    let method = jstr(&mut env, &method);
    let args: Value = serde_json::from_str(&jstr(&mut env, &args)).unwrap_or(Value::Null);
    let Some(glue) = GLUE.get().cloned() else { return };
    let Some(session) = glue.session.get().cloned() else {
        glue.reply(id, Err("the app has not started".into()));
        return;
    };
    let g = glue.clone();
    let started = catch_unwind(AssertUnwindSafe(|| session.call_then(method, args, move |res| g.reply(id, res))));
    if started.is_err() {
        glue.reply(id, Err("internal error".into()));
    }
}

/// `Core.nativeStop()`: flush caches and stop the backend (the app is being destroyed).
#[no_mangle]
pub extern "system" fn Java_lol_agu_chat_Core_nativeStop(_env: JNIEnv, _c: JClass) {
    if let Some(s) = GLUE.get().and_then(|g| g.session.get()) {
        let _ = catch_unwind(AssertUnwindSafe(|| s.shutdown()));
    }
}
