package lol.agu.chat.ui

import androidx.compose.runtime.Immutable
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonPrimitive

@Serializable
@Immutable
data class RowUser(val login: String? = null, val name: String? = null, val color: String? = null)

@Serializable
@Immutable
data class Badge(val url: String, val title: String? = null)

@Serializable
@Immutable
data class Overlay(val name: String? = null, val url: String)

/** One piece of a message; [t] is text / emote / emoji / mention / link / gif / cheer, the rest depends on it. */
@Serializable
@Immutable
data class Part(
    val t: String,
    val s: String? = null,
    val name: String? = null,
    val url: String? = null,
    val alt: String? = null,
    val login: String? = null,
    val color: String? = null,
    val amount: Double? = null,
    val overlays: List<Overlay> = emptyList(),
)

@Serializable
@Immutable
data class ReplyRef(val user: String? = null, val text: String? = null, val parentId: JsonPrimitive? = null)

@Serializable
@Immutable
data class Row(
    val key: String,
    val sk: String = "",
    val platform: String = "",
    val ts: Long = 0,
    val backlog: Boolean = false,
    val kind: String = "msg",
    val id: JsonPrimitive? = null,
    val user: RowUser? = null,
    val badges: List<Badge> = emptyList(),
    val parts: List<Part> = emptyList(),
    val action: Boolean = false,
    val firstMsg: Boolean = false,
    val deleted: Boolean = false,
    val deletedNote: String? = null,
    val reply: ReplyRef? = null,
    val system: String? = null,
    val bits: Double? = null,
    val status: String? = null,
) {
    /** The text of the message as plain text, for copying. */
    fun plainText(): String = parts.joinToString("") {
        when (it.t) {
            "emote" -> it.name.orEmpty()
            "gif" -> it.alt ?: it.url.orEmpty()
            "cheer" -> it.name.orEmpty() + (it.amount?.toLong()?.toString() ?: "")
            else -> it.s.orEmpty()
        }
    }
}

@Serializable
@Immutable
data class Conn(
    val state: String = "connecting",
    val detail: String? = null,
    val live: Boolean? = null,
    val title: String? = null,
    val canSend: Boolean? = null,
    val readOnlyReason: String? = null,
    val loggedOut: Boolean? = null,
)

@Serializable
@Immutable
data class AccountUser(val id: JsonPrimitive? = null, val login: String? = null, val name: String? = null)

@Serializable
@Immutable
data class Pending(val userCode: String = "", val verificationUri: String = "")

@Serializable
@Immutable
data class AccountState(
    val connected: Boolean = false,
    val user: AccountUser? = null,
    val pending: Pending? = null,
    val error: String? = null,
    val unsupported: Boolean = false,
)

@Serializable
@Immutable
data class SavedTab(val platform: String, val channel: String)

/** An open channel tab; [sk] is the core's source key for its chat. */
@Immutable
data class Tab(val platform: String, val channel: String, val sk: String) {
    val id get() = "$platform:${channel.lowercase()}"
}

/** The core wants a sign-in screen: a WebView on [url]. [show] means "point the open window here" (Twitch's authorise page). */
@Immutable
data class LoginRequest(val id: Long, val platform: String, val url: String, val show: Boolean = false)
