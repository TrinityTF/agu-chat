package lol.agu.chat

import android.content.Context
import android.content.Intent
import android.net.Uri
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/** What the Rust side calls back with (on its own threads). */
interface CoreListener {
    fun onEvent(kind: String, json: String)
    fun onReply(id: Long, ok: Boolean, json: String)
    fun openUrl(url: String)
}

/** One event from the Rust side: `rows`, `conn`, `accounts` or `settings`, with its JSON payload. */
data class CoreEvent(val kind: String, val data: JsonElement)

class CoreError(message: String) : Exception(message)

/**
 * The Rust half of the app (native/android): the same core and chat logic as the desktop app.
 * Calls are JSON in, JSON out; events stream through [events].
 */
object Core {
    val json = Json { ignoreUnknownKeys = true; explicitNulls = false; encodeDefaults = true }

    private val nextId = AtomicLong(1)
    private val waiting = ConcurrentHashMap<Long, CompletableDeferred<JsonElement>>()
    private val _events = MutableSharedFlow<CoreEvent>(extraBufferCapacity = 1024)
    val events: SharedFlow<CoreEvent> = _events
    @Volatile private var started = false

    @JvmStatic private external fun nativeStart(dataDir: String, key: ByteArray, version: String, listener: CoreListener)
    @JvmStatic private external fun nativeCall(id: Long, method: String, args: String)
    @JvmStatic private external fun nativeStop()

    fun start(context: Context) {
        if (started) return
        started = true
        System.loadLibrary("agu_chat_android")
        val app = context.applicationContext
        val version = app.packageManager.getPackageInfo(app.packageName, 0).versionName ?: "0.0.0"
        val listener = object : CoreListener {
            override fun onEvent(kind: String, json: String) {
                val data = runCatching { Core.json.parseToJsonElement(json) }.getOrElse { JsonNull }
                _events.tryEmit(CoreEvent(kind, data))
            }

            override fun onReply(id: Long, ok: Boolean, json: String) {
                val d = waiting.remove(id) ?: return
                val v = runCatching { Core.json.parseToJsonElement(json) }.getOrElse { JsonNull }
                if (ok) d.complete(v) else d.completeExceptionally(CoreError((v as? JsonPrimitive)?.content ?: "failed"))
            }

            override fun openUrl(url: String) {
                runCatching {
                    app.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                }
            }
        }
        nativeStart(app.filesDir.resolve("agu").absolutePath, KeyVault.secretsKey(app), version, listener)
    }

    /** Ask the Rust side something; throws [CoreError] with its message when it says no. */
    suspend fun call(method: String, args: JsonObject = JsonObject(emptyMap())): JsonElement {
        val id = nextId.getAndIncrement()
        val d = CompletableDeferred<JsonElement>()
        waiting[id] = d
        nativeCall(id, method, args.toString())
        return d.await()
    }

    fun stop() {
        if (started) nativeStop()
    }
}
