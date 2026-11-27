package lol.agu.chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.Placeholder
import androidx.compose.ui.text.PlaceholderVerticalAlign
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.material3.Text
import androidx.compose.foundation.text.InlineTextContent
import androidx.compose.foundation.text.appendInlineContent
import coil3.compose.AsyncImage
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/** Image url -> width/height, filled in when Coil has decoded it so emote placeholders get the right width. */
private val aspects = mutableStateMapOf<String, Float>()

private val timeFmt = DateTimeFormatter.ofPattern("HH:mm").withZone(ZoneId.systemDefault())
private const val EmoteDp = 28f
private const val BadgeDp = 16f
private const val GifDp = 120f
private const val MarkDp = 15f

/** What an inline placeholder shows; the placeholder size is resolved at composition time from [aspects]. */
private class InlineSpec(val url: String, val overlays: List<String>, val heightDp: Float, val mark: String? = null)

private class Built(val text: AnnotatedString, val inline: Map<String, InlineSpec>)

fun parseColor(hex: String?): Color? = hex?.let {
    runCatching { Color(android.graphics.Color.parseColor(it)) }.getOrNull()
}

private fun build(row: Row, showMark: Boolean): Built {
    val inline = LinkedHashMap<String, InlineSpec>()
    val nameColor = parseColor(row.user?.color) ?: AguColors.Text
    val name = row.user?.name?.takeIf { it.isNotEmpty() } ?: row.user?.login
    var n = 0
    val text = buildAnnotatedString {
        withStyle(SpanStyle(color = AguColors.Muted, fontSize = 11.sp)) { append(timeFmt.format(Instant.ofEpochMilli(row.ts))); append(" ") }
        // in a merged tab every line says which platform it came from
        if (showMark && row.platform.isNotEmpty()) {
            inline["p"] = InlineSpec("", emptyList(), MarkDp, mark = row.platform)
            appendInlineContent("p", "•")
            append(" ")
        }
        for (b in row.badges) {
            val id = "b${n++}"
            inline[id] = InlineSpec(b.url, emptyList(), BadgeDp)
            appendInlineContent(id, "■")
            append(" ")
        }
        if (name != null) {
            withStyle(SpanStyle(color = nameColor, fontWeight = FontWeight.Bold)) { append(name) }
            if (!row.action && row.parts.isNotEmpty()) append(": ") else append(" ")
        }
        val bodyStyle = when {
            row.deleted -> SpanStyle(color = AguColors.Muted, textDecoration = TextDecoration.LineThrough)
            row.action -> SpanStyle(color = nameColor)
            else -> SpanStyle()
        }
        withStyle(bodyStyle) {
            for (p in row.parts) when (p.t) {
                "emote" -> {
                    val id = "e${n++}"
                    inline[id] = InlineSpec(p.url.orEmpty(), p.overlays.map { it.url }, EmoteDp)
                    appendInlineContent(id, p.name ?: "?")
                }
                "gif" -> {
                    val id = "g${n++}"
                    inline[id] = InlineSpec(p.url.orEmpty(), emptyList(), GifDp)
                    appendInlineContent(id, p.alt ?: "gif")
                }
                "cheer" -> {
                    if (!p.url.isNullOrEmpty()) {
                        val id = "c${n++}"
                        inline[id] = InlineSpec(p.url, emptyList(), BadgeDp + 4)
                        appendInlineContent(id, p.name ?: "cheer")
                    }
                    withStyle(SpanStyle(color = parseColor(p.color) ?: AguColors.Accent, fontWeight = FontWeight.Bold)) {
                        append(p.amount?.toLong()?.toString() ?: "")
                    }
                }
                "mention" -> withStyle(SpanStyle(fontWeight = FontWeight.Bold, color = parseColor(p.color) ?: AguColors.Text)) { append(p.s.orEmpty()) }
                "link" -> {
                    val style = TextLinkStyles(SpanStyle(color = AguColors.Accent, textDecoration = TextDecoration.Underline))
                    if (p.url.isNullOrEmpty()) append(p.s.orEmpty())
                    else withLink(LinkAnnotation.Url(p.url, style)) { append(p.s ?: p.url) }
                }
                else -> append(p.s.orEmpty()) // text and emoji
            }
        }
        if (row.deleted && !row.deletedNote.isNullOrEmpty()) {
            append(" ")
            withStyle(SpanStyle(color = AguColors.Muted, fontStyle = FontStyle.Italic)) { append(row.deletedNote) }
        }
    }
    return Built(text, inline)
}

/** Resolves the placeholders against the current aspect cache; reading it here subscribes the row to changes. */
private fun inlineContent(spec: Map<String, InlineSpec>, density: Density): Map<String, InlineTextContent> =
    spec.mapValues { (_, s) ->
        if (s.mark != null) {
            val side = with(density) { s.heightDp.dp.toSp() }
            return@mapValues InlineTextContent(Placeholder(side, side, PlaceholderVerticalAlign.TextCenter)) { PlatformMark(s.mark) }
        }
        val aspect = (aspects[s.url] ?: 1f).coerceIn(0.2f, 8f)
        val h = with(density) { s.heightDp.dp.toSp() }
        val w = with(density) { (s.heightDp * aspect).dp.toSp() }
        InlineTextContent(Placeholder(w, h, PlaceholderVerticalAlign.TextCenter)) {
            Box(Modifier.fillMaxSize()) {
                AsyncImage(
                    model = s.url,
                    contentDescription = null,
                    contentScale = ContentScale.Fit,
                    modifier = Modifier.fillMaxSize(),
                    onSuccess = { st ->
                        val img = st.result.image
                        if (img.height > 0) aspects[s.url] = img.width.toFloat() / img.height
                    },
                )
                // Zero-width emotes sit on top of the base emote.
                for (o in s.overlays) AsyncImage(model = o, contentDescription = null, contentScale = ContentScale.Fit, modifier = Modifier.fillMaxSize())
            }
        }
    }

private val bodyStyle = TextStyle(color = AguColors.Text, fontSize = 15.sp)

@Composable
fun MessageRow(row: Row, onLongPress: (Row) -> Unit, modifier: Modifier = Modifier, showMark: Boolean = false, flash: Boolean = false, onTap: (() -> Unit)? = null) {
    val built = remember(row, showMark) { build(row, showMark) }
    val density = LocalDensity.current
    val longPress by rememberUpdatedState(onLongPress)

    // a line naming you is red, ahead of a highlight rule's colour (the desktop's order)
    val rule = row.highlight?.let { h -> parseColor(h.color)?.let { it to h.alpha } }
    val barColor = when {
        row.mention -> MentionRed
        row.kind == "notice" -> AguColors.Purple
        row.kind == "automod" -> AguColors.Amber
        else -> rule?.first
    }
    var m = modifier
        .fillMaxWidth()
        .pointerInput(row.key) { detectTapGestures(onTap = onTap?.let { t -> { _ -> t() } }, onLongPress = { longPress(row) }) }
    if (row.firstMsg) m = m.background(AguColors.Green.copy(alpha = 0.10f))
    if (row.mention) m = m.background(MentionRed.copy(alpha = 0.12f))
    else if (rule != null && row.kind != "notice" && row.kind != "automod") m = m.background(rule.first.copy(alpha = rule.second))
    if (flash) m = m.background(AguColors.Amber.copy(alpha = 0.30f))
    if (barColor != null) m = m.drawBehind { drawRect(barColor, size = Size(3.dp.toPx(), size.height)) }
    Column(m.padding(start = if (barColor != null) 11.dp else 8.dp, end = 8.dp, top = 3.dp, bottom = 3.dp)) {
        if (row.mention) Text("MENTIONS YOU", color = MentionLabel, fontSize = 10.sp, fontWeight = FontWeight.Bold)
        row.reply?.let { r ->
            Text(
                "Replying to @${r.user.orEmpty()}: ${r.text.orEmpty()}",
                color = AguColors.Muted, fontSize = 12.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
        }
        if (row.kind == "modfeed") {
            val line = row.system ?: row.plainText()
            Text("${timeFmt.format(Instant.ofEpochMilli(row.ts))}  $line", color = AguColors.Muted, fontSize = 12.sp, fontStyle = FontStyle.Italic)
            return@Column
        }
        if (!row.system.isNullOrEmpty()) {
            Text(
                row.system,
                color = AguColors.Muted,
                fontSize = 12.sp,
                fontWeight = if (row.kind == "msg") FontWeight.Normal else FontWeight.Bold,
            )
        }
        // Notices and automod lines without a body only show the system text above.
        if (row.parts.isNotEmpty() || row.kind == "msg" && row.system.isNullOrEmpty()) {
            Text(built.text, inlineContent = inlineContent(built.inline, density), style = bodyStyle)
        }
    }
}

private val MentionRed = Color(0xFFFF3B3B)
private val MentionLabel = Color(0xFFFCA5A5)

/** The small platform-coloured mark in front of a line of a merged tab. */
@Composable
fun PlatformMark(platform: String, modifier: Modifier = Modifier) {
    val letter = when (platform) { "twitch" -> "T"; "kick" -> "K"; "youtube" -> "Y"; "rumble" -> "R"; "x" -> "X"; else -> "?" }
    val dark = platform == "kick" || platform == "rumble" || platform == "x"
    Box(
        modifier.fillMaxSize().clip(androidx.compose.foundation.shape.RoundedCornerShape(3.dp)).background(platformColor(platform)),
        contentAlignment = androidx.compose.ui.Alignment.Center,
    ) {
        Text(letter, color = if (dark) Color.Black else Color.White, fontSize = 9.sp, fontWeight = FontWeight.Black, lineHeight = 9.sp)
    }
}
