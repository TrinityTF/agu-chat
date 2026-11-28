package lol.agu.chat

import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive

/**
 * The mentions list as last seen, on disk. The session keeps it in memory only, and Android can end the app in the
 * background: the notification was still there, but the bell's list came back empty. The saved entries go under the
 * session's own list (newest first, the same 200 cap as the session).
 */
object MentionStore {
    private const val MAX = 200
    private val lock = Mutex()

    private fun id(e: JsonElement) = (e as? JsonObject)?.get("id")?.jsonPrimitive?.contentOrNull
    private fun ts(e: JsonElement) = (e as? JsonObject)?.get("ts")?.jsonPrimitive?.contentOrNull?.toLongOrNull() ?: 0L

    /** [live] (the `mentions.list` answer) with the saved ones it no longer has; what comes back is also saved. */
    suspend fun merged(ctx: Context, live: JsonArray): JsonArray = withContext(Dispatchers.IO) {
        lock.withLock {
            val file = java.io.File(ctx.filesDir, "mentions.json")
            val saved = runCatching { Core.json.parseToJsonElement(file.readText()) as JsonArray }.getOrNull() ?: JsonArray(emptyList())
            val have = live.mapNotNull(::id).toSet()
            val out = JsonArray((live + saved.filter { id(it) !in have }).sortedByDescending(::ts).take(MAX))
            if (out != saved) runCatching { file.writeText(out.toString()) }
            out
        }
    }

    /** Saves the session's current list (a mention came in, or one left). */
    suspend fun refresh(ctx: Context) {
        val live = runCatching { Core.call("mentions.list") as JsonArray }.getOrNull() ?: return
        merged(ctx, live)
    }
}
