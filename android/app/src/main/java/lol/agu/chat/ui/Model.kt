package lol.agu.chat.ui

import androidx.compose.runtime.Immutable
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonPrimitive

/** A 7TV name paint: a gradient (colour stops, an angle), a solid colour, plus drop shadows. Colours are `#aarrggbb`. */
@Serializable
@Immutable
data class PaintStop(val at: Float = 0f, val color: String = "#ffffffff")

@Serializable
@Immutable
data class PaintShadow(val x: Float = 0f, val y: Float = 0f, val radius: Float = 0f, val color: String = "#ff000000")

@Serializable
@Immutable
data class Paint(
    val function: String = "",
    val color: String? = null,
    val angle: Float = 0f,
    val repeat: Boolean = false,
    val shape: String = "",
    val stops: List<PaintStop> = emptyList(),
    val shadows: List<PaintShadow> = emptyList(),
)

@Serializable
@Immutable
data class RowUser(val id: String? = null, val login: String? = null, val name: String? = null, val color: String? = null, val paint: Paint? = null, val alias: String? = null, val avatar: String? = null)

/** A badge: a picture, or (Kick and YouTube's own) a small glyph chip. [title] shows on long-press. */
@Serializable
@Immutable
data class Badge(val url: String? = null, val title: String? = null, val glyph: String? = null, val color: String? = null)

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
    /** the emote's big picture, its provider and the line the long-press popup shows */
    val tip: String? = null,
    val provider: String? = null,
    val label: String? = null,
    val tipName: String? = null,
    val tipLabel: String? = null,
)

@Serializable
@Immutable
data class ReplyRef(val user: String? = null, val text: String? = null, val parentId: JsonPrimitive? = null, val threadId: JsonPrimitive? = null) {
    /** Tapping the line opens the thread: the reply has to say which message it answers. */
    val opensThread: Boolean get() = !parentId?.content.isNullOrEmpty() || !threadId?.content.isNullOrEmpty()
}

/** A matched highlight rule: the row's colour, and how strong its tint is. */
@Serializable
@Immutable
data class Highlight(val color: String, val alpha: Float = 0.14f)

/** The header of a redeem, a cheer or a Kick gift: who did what, and for how much (the right side). */
@Serializable
@Immutable
data class EventHead(
    val verb: String = "",
    val reward: String? = null,
    val amount: Double? = null,
    val amountText: String? = null,
    val bits: Boolean = false,
    val bitsColor: String? = null,
    val kicks: Boolean = false,
    val image: String? = null,
)

/** The app's own line about a live 7TV emote change. */
@Serializable
@Immutable
data class SevenNote(val actor: String? = null, val verb: String = "", val url: String = "", val name: String = "", val oldName: String = "", val action: String = "")

/** A system line: a sub, a gift, a raid, an announcement, a Super Chat, a membership. */
@Serializable
@Immutable
data class NoticeInfo(
    val icon: String? = null,
    val iconSize: Int = 16,
    /** the name on its own line above the sentence, in [headColor] */
    val head: String? = null,
    val headColor: String? = null,
    val sentence: String? = null,
    /** the part of [sentence] shown bold (start, end) */
    val bold: List<Int>? = null,
    val raid: Boolean = false,
    /** the bar's colour (`#aarrggbb`) when it is not the accent: an announcement's, a Super Chat's */
    val bar: String? = null,
    /** a Reply button puts @name in the message box */
    val reply: Boolean = false,
    val announce: Boolean = false,
    val superChat: Boolean = false,
    val member: Boolean = false,
    val seventv: SevenNote? = null,
)

/** The first link of a message that a card can be made for. */
@Serializable
@Immutable
data class LinkRef(val href: String, val key: String, val provider: String = "")

@Serializable
@Immutable
data class Rant(val text: String = "", val color: String = "#00bcff")

/** What a link preview card shows (the core's `Preview`). */
@Serializable
@Immutable
data class Preview(val key: String = "", val provider: String = "", val title: String = "", val subtitle: String = "", val thumb: String? = null)

/** Who and what a moderation action is about (the core's `ModTarget`): the message, its sender, YouTube's menu params. */
@Serializable
@Immutable
data class ModTarget(val id: String = "", val uid: String = "", val login: String = "", val name: String = "", val menu: String? = null)

/** What the signed-in account may do to this line (the core decides by platform, caps and badges), and the timeout lengths. */
@Serializable
@Immutable
data class ModInfo(
    val t: ModTarget = ModTarget(),
    val ban: Boolean = false,
    val timeout: Boolean = false,
    val warn: Boolean = false,
    val delete: Boolean = false,
    val unban: Boolean = false,
    /** Rumble: "Pin this message" */
    val pin: Boolean = false,
    /** a command button (`/announce ...`) may run */
    val cmd: Boolean = false,
    /** (label, seconds) */
    val ladder: List<List<kotlinx.serialization.json.JsonPrimitive>> = emptyList(),
    /** Rumble's mute choices: (label, action, seconds) */
    val muteChoices: List<List<kotlinx.serialization.json.JsonPrimitive>> = emptyList(),
) {
    val choices: List<Pair<String, Int>> get() = ladder.mapNotNull { l -> if (l.size == 2) l[0].content to (l[1].content.toIntOrNull() ?: return@mapNotNull null) else null }
}

/** What the account may do in a channel: shows the mod mode toggle and the channel modes button. */
@Immutable
data class Caps(val mod: Boolean = false, val modes: Boolean = false)

/** One mod mode button (settings -> Mod buttons): [action] is ban | timeout | warn | delete | command. */
@Serializable
@Immutable
data class ModButtonCfg(
    val id: String = "",
    val action: String = "ban",
    val icon: String = "",
    val label: String = "",
    val seconds: Int = 0,
    val command: String = "",
)

@Serializable
@Immutable
data class Row(
    val key: String,
    val sk: String = "",
    val platform: String = "",
    val ts: Long = 0,
    /** arrival counter: alternating row shading keys off it */
    val ord: Long = 0,
    val backlog: Boolean = false,
    val kind: String = "msg",
    val id: JsonPrimitive? = null,
    val user: RowUser? = null,
    val badges: List<Badge> = emptyList(),
    val parts: List<Part> = emptyList(),
    val action: Boolean = false,
    val firstMsg: Boolean = false,
    val returning: Boolean = false,
    val highlighted: Boolean = false,
    val deleted: Boolean = false,
    val deletedNote: String? = null,
    val reply: ReplyRef? = null,
    val system: String? = null,
    val bits: Double? = null,
    val status: String? = null,
    val chip: String? = null,
    val resolved: String? = null,
    val event: EventHead? = null,
    val notice: NoticeInfo? = null,
    val link: LinkRef? = null,
    val rant: Rant? = null,
    /** a highlight rule matched this line */
    val highlight: Highlight? = null,
    /** the line names you (drawn red) */
    val mention: Boolean = false,
    /** what a moderator may do to this line; null when nothing */
    val mod: ModInfo? = null,
    /** an AutoMod hold: Allow / Deny work for this account */
    val canResolve: Boolean = false,
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

/** One channel of a tab. */
@Serializable
@Immutable
data class ChannelRef(val platform: String, val channel: String)

@Serializable
@Immutable
data class SavedTab(val name: String = "", val channels: List<ChannelRef> = emptyList(), val modMode: Boolean = false)

/** The core's source key of a channel (Twitch and Kick names are kept lowercase). */
fun ChannelRef.sk(): String = "$platform:${if (platform == "twitch" || platform == "kick") channel.lowercase() else channel}"

/** A Twitch whisper conversation is a reserved channel, `whisper:<login>` (the core's `twitch/whisper.rs`). */
fun ChannelRef.isWhisper(): Boolean = platform == "twitch" && channel.startsWith("whisper:")

/** What a channel is called on screen: a whisper conversation shows the other person. */
fun ChannelRef.shown(): String = if (isWhisper()) channel.removePrefix("whisper:") else channel

/** `isWhisper` for a source key (`twitch:whisper:bob`). */
fun isWhisperSk(sk: String): Boolean = sk.startsWith("twitch:whisper:")

/** The channel in a sentence ("Twitch agu", "whisper bob"). */
fun ChannelRef.label(): String = if (isWhisper()) "whisper ${shown()}" else "${platformLabel(platform)} $channel"

/**
 * An open tab: one or more channels from any platforms merged into one list. [id] is the core's view key; [name] is
 * the user's own title ("" shows the channels' names).
 */
@Immutable
data class Tab(val id: String, val name: String, val channels: List<ChannelRef>) {
    val sks get() = channels.map { it.sk() }

    /** The title in the tab strip: the name, else the first channel (and "…" when there are more). */
    fun title(full: Boolean = false): String = when {
        name.isNotBlank() -> name
        full || channels.size < 2 -> channels.joinToString(", ") { it.shown() }
        else -> channels.first().shown() + ", …"
    }

    /** A tab that holds nothing but whisper conversations (its mark is an envelope). */
    val whisperOnly get() = channels.isNotEmpty() && channels.all { it.isWhisper() }
}

/** What a tab's small circle says; a mention beats live, live beats new messages. */
enum class Dot { Message, Live, Highlight }

/** One line of the Mentions screen. */
@Immutable
data class MentionEntry(val id: String, val vk: String, val sk: String, val channel: String, val platform: String, val ts: Long, val read: Boolean, val whisper: Boolean, val row: Row)

/** One Twitch channel's hype train bar (the core's `hype` event, see `hype_json` in native/android/src/session.rs). */
@Serializable
@Immutable
data class HypeEmoteInfo(val token: String = "", val url: String = "")

@Serializable
@Immutable
data class HypeConductor(val name: String = "", val avatar: String = "", val what: String = "")

@Serializable
@Immutable
data class HypeTrain(
    val id: String = "",
    val level: Int = 1,
    val percent: Int = 0,
    /** unix ms; null when Twitch sent no end time */
    val expiresMs: Long? = null,
    /** the bar's fill, `#rrggbb` */
    val fill: String = "#9147ff",
    /** what is left of the level, as text: "3,300 Bits", "7 Subs" */
    val bits: String = "",
    val subs: String = "",
    val emotes: List<HypeEmoteInfo> = emptyList(),
    val info: List<String> = emptyList(),
    val conductors: List<HypeConductor> = emptyList(),
)

/** Show this row of this tab (a tapped mention), once. */
@Immutable
data class Jump(val vk: String, val key: String, val nonce: Long)

/** The core wants a sign-in screen: a WebView on [url]. [show] means "point the open window here" (Twitch's authorise page). */
@Immutable
data class LoginRequest(val id: Long, val platform: String, val url: String, val show: Boolean = false)

/** The answer to a moderation call: [toast] is what to say (a warning when [warn]); [confirm] is a question to ask before a ban. */
@Immutable
data class ModResult(val ok: Boolean, val toast: String? = null, val warn: Boolean = false, val confirm: String? = null)

/** Rumble's mute choices as (label, action, seconds), from the core's `[[label, action, seconds]]`. */
fun muteChoicesOf(raw: List<List<JsonPrimitive>>): List<Triple<String, String, Int>> =
    raw.mapNotNull { l -> if (l.size == 3) Triple(l[0].content, l[1].content, l[2].content.toIntOrNull() ?: 0) else null }

/** A Rumble chat's pinned message, drawn as a bar at the top of the chat ([canMod]: the account may unpin it). */
@Immutable
class RumblePin(val row: Row, val canMod: Boolean)

/** A Twitch channel's pinned message: the line drawn like a chat line, who pinned it, when it ends ([endsAtMs]), and whether this account may unpin it. */
@Immutable
class TwitchPin(val id: String, val row: Row, val byName: String?, val byBadges: List<Badge>, val endsAtMs: Long?, val canMod: Boolean)

/** One reply thread to show: the chat and the line it was opened from (see `thread.rows`). */
@Immutable
data class ThreadReq(val vk: String, val sk: String, val parent: String, val thread: String)

/** What `thread.rows` answers: the lines of the thread, whether its first message is still in the chat, and whether a reply can be posted here. */
@Immutable
class ThreadData(val root: String, val hasRoot: Boolean, val canReply: Boolean, val rows: List<Row>)

/** One entry of Rumble's muted list; [scope] is "video" (this livestream) or "total". */
@Immutable
data class MutedUser(val name: String, val id: String = "", val scope: String = "")
