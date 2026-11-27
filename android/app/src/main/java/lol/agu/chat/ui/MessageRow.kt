package lol.agu.chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row as LayoutRow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.InlineTextContent
import androidx.compose.foundation.text.appendInlineContent
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shadow
import androidx.compose.ui.graphics.TileMode
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.Placeholder
import androidx.compose.ui.text.PlaceholderVerticalAlign
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.TextMeasurer
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import coil3.compose.AsyncImage
import coil3.decode.BitmapFactoryDecoder
import coil3.request.ImageRequest
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.max
import kotlin.math.sin

/** Image url -> width/height, filled in when Coil has decoded it so emote placeholders get the right width. */
private val aspects = mutableStateMapOf<String, Float>()

/** Badge pictures that would not load: their place closes up, so the badges and the name sit together. */
private val failedBadges = mutableStateMapOf<String, Boolean>()

private const val GifDp = 120f
private const val MarkDp = 15f

private val Faint = Color(0xFF71717A)
private val MentionRed = Color(0xFFFF3B3B)
private val MentionLabel = Color(0xFFFCA5A5)
private val OrangeText = Color(0xFFFFD6A7)
private val RaidOrange = Color(0xFFFF6900)
private val EventPurple = Color(0xFFAD46FF)
private val AccentSoft = Color(0xFFA970FF)
private val FirstPurple = Color(0xFF9147FF)
private val FirstLabel = Color(0xFFDAB2FF)
private val ReturnBlue = Color(0xFF2B8FEF)
private val ReturnLabel = Color(0xFF74D4FF)
private val SlateBar = Color(0xFF64748B)
private val AmberBar = Color(0xFFF59E0B)
private val AmberText = Color(0xFFFEE685)
private val KickGreen = Color(0xFF53FC18)
private val MemberGreen = Color(0xFF2BA640)

fun parseColor(hex: String?): Color? = hex?.takeIf { it.isNotEmpty() }?.let {
    runCatching { Color(android.graphics.Color.parseColor(it)) }.getOrNull()
}

/** What a long-press on an emote, a badge or an emoji shows: its picture (or glyph), its name and where it is from. */
class Tip(val name: String, val label: String, val img: String? = null, val glyph: String? = null)

/** What an inline placeholder shows; the placeholder size is resolved at composition time. */
private sealed interface Inl {
    class Img(val url: String, val overlays: List<String>, val heightDp: Float, val tip: Tip?, val badge: Boolean = false) : Inl
    class Mark(val platform: String) : Inl
    class Chip(val glyph: String, val color: Color, val tip: Tip?) : Inl
    class Emoji(val text: String, val overlays: List<String>, val tip: Tip?) : Inl
    class PaintName(val name: String, val paint: Paint, val base: Color, val onClick: (() -> Unit)? = null) : Inl
    class Gif(val url: String) : Inl
    /** a mod mode button at the start of the line (the desktop draws them inline too) */
    class ModBtn(val b: ModButtonCfg, val onClick: () -> Unit) : Inl
}

/** Mod mode on a chat line: the buttons go inline at the start of its text, like the desktop's. */
private class InlineMod(val buttons: List<ModButtonCfg>, val run: (ModButtonCfg) -> Unit)

/** Opens the reply thread of a line (a tap on its "Replying to" line); null where that is not offered. */
val LocalThreadOpener = androidx.compose.runtime.compositionLocalOf<((Row) -> Unit)?> { null }

/** The "↩ Replying to @name: ..." line above a reply; a tap opens the thread when the reply says which message it answers. */
@Composable
private fun ReplyLine(row: Row, r: ReplyRef, d: Display) {
    val open = LocalThreadOpener.current
    Text(
        "Replying to @${r.user.orEmpty()}: ${r.text.orEmpty()}", color = AguColors.Muted, fontSize = (d.bodySp * 0.8f).sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
        modifier = if (open != null && r.opensThread) Modifier.clickable { open(row) } else Modifier,
    )
}

/** A short line of [lead] muted text, then badges and a name, then [trail]: the pinned card's "Pinned by ..." header. */
@Composable
fun BadgedName(lead: String, badges: List<Badge>, name: String?, nameColor: Color, trail: String, fontSp: Float, modifier: Modifier = Modifier) {
    val d = LocalDisplay.current
    val built = remember(lead, badges, name, nameColor, trail, d) {
        val b = Bld(d)
        val text = buildAnnotatedString {
            withStyle(SpanStyle(color = AguColors.Muted)) { append(lead) }
            badges(b, badges, d)
            if (name != null) withStyle(SpanStyle(color = nameColor, fontWeight = FontWeight.SemiBold)) { append(name) }
            if (trail.isNotEmpty()) withStyle(SpanStyle(color = Faint)) { append(trail) }
        }
        Built(text, b.inline)
    }
    val density = LocalDensity.current
    val measurer = rememberTextMeasurer()
    Text(built.text, inlineContent = inlineContent(built.inline, density, d, measurer) {}, style = TextStyle(color = AguColors.Text, fontSize = fontSp.sp), modifier = modifier)
}

private val LocalInlineMod = androidx.compose.runtime.compositionLocalOf<InlineMod?> { null }

private class Built(val text: AnnotatedString, val inline: Map<String, Inl>)

/** The pieces of building one line: the display settings and the placeholders collected so far. */
private class Bld(val d: Display) {
    val inline = LinkedHashMap<String, Inl>()
    private var n = 0
    fun id(prefix: String) = "$prefix${n++}"
    fun <T : Inl> add(prefix: String, i: T): String = id(prefix).also { inline[it] = i }
}

private fun emoteTip(p: Part) = Tip(p.name.orEmpty(), p.label ?: p.provider.orEmpty(), p.tip?.takeIf { it.isNotEmpty() } ?: p.url)

/** The parts of a message (or a note under a notice) appended to [b]'s text. */
private fun AnnotatedString.Builder.parts(b: Bld, list: List<Part>, d: Display) {
    for (p in list) when (p.t) {
        "emote" -> {
            val id = b.add("e", Inl.Img(p.url.orEmpty(), p.overlays.map { it.url }, d.emoteDp, emoteTip(p)))
            appendInlineContent(id, p.name ?: "?")
        }
        "emoji" -> {
            val id = b.add("j", Inl.Emoji(p.s.orEmpty(), p.overlays.map { it.url }, Tip(p.tipName ?: p.s.orEmpty(), p.tipLabel ?: "Emoji", glyph = p.s)))
            appendInlineContent(id, p.s.orEmpty())
        }
        "gif" -> {
            val id = b.add("g", Inl.Gif(p.url.orEmpty()))
            appendInlineContent(id, p.alt ?: "gif")
        }
        "cheer" -> {
            if (!p.url.isNullOrEmpty()) {
                val id = b.add("c", Inl.Img(p.url, emptyList(), d.emoteDp, Tip(p.name.orEmpty(), p.label ?: "Cheermote", p.url)))
                appendInlineContent(id, p.name ?: "cheer")
            }
            withStyle(SpanStyle(color = parseColor(p.color) ?: AguColors.Accent, fontWeight = FontWeight.Bold)) {
                append(p.amount?.toLong()?.toString() ?: "")
            }
        }
        "mention" -> withStyle(SpanStyle(fontWeight = FontWeight.SemiBold, color = parseColor(p.color) ?: AguColors.Text)) { append(p.s.orEmpty()) }
        "link" -> {
            val style = TextLinkStyles(SpanStyle(color = AccentSoft, textDecoration = TextDecoration.Underline))
            if (p.url.isNullOrEmpty()) append(p.s.orEmpty())
            else withLink(LinkAnnotation.Url(p.url, style)) { append(p.s ?: p.url) }
        }
        else -> append(p.s.orEmpty())
    }
}

private fun AnnotatedString.Builder.badges(b: Bld, list: List<Badge>, d: Display) {
    if (!d.showBadges) return
    for (bd in list) {
        val tip = Tip(bd.title.orEmpty(), "Badge", bd.url)
        val id = if (!bd.url.isNullOrEmpty()) b.add("b", Inl.Img(bd.url, emptyList(), d.badgeDp, tip, badge = true))
        else b.add("k", Inl.Chip(bd.glyph.orEmpty(), parseColor(bd.color) ?: AguColors.Muted, tip))
        appendInlineContent(id, "■")
        append(" ")
    }
}

/** A chat line: time, platform mark, badges, name, then the message. [full] false leaves the name off (a note). */
private fun build(row: Row, d: Display, showMark: Boolean, time: String?, onUser: (() -> Unit)? = null, mod: InlineMod? = null): Built {
    val b = Bld(d)
    val nameColor = parseColor(row.user?.color) ?: AguColors.Text
    val name = row.user?.name?.takeIf { it.isNotEmpty() } ?: row.user?.login
    val grey = row.deleted && d.deletedMessages == "strike"
    val text = buildAnnotatedString {
        mod?.let { m ->
            for (btn in m.buttons) appendInlineContent(b.add("m", Inl.ModBtn(btn) { m.run(btn) }), "■")
            if (m.buttons.isNotEmpty()) append(" ")
        }
        if (time != null) withStyle(SpanStyle(color = Faint, fontSize = (d.bodySp * 0.85f).sp)) { append(time); append(" ") }
        // in a merged tab every line says which platform it came from
        if (showMark && row.platform.isNotEmpty()) {
            appendInlineContent(b.add("p", Inl.Mark(row.platform)), "•")
            append(" ")
        }
        badges(b, row.badges, d)
        if (name != null) {
            val paint = row.user?.paint
            if (paint != null && !row.action) appendInlineContent(b.add("n", Inl.PaintName(name, paint, nameColor, onUser)), name)
            else if (onUser != null) withLink(LinkAnnotation.Clickable("user", TextLinkStyles(SpanStyle(textDecoration = TextDecoration.None)), linkInteractionListener = { onUser() })) { withStyle(SpanStyle(color = nameColor, fontWeight = FontWeight.Bold)) { append(name) } }
            else withStyle(SpanStyle(color = nameColor, fontWeight = FontWeight.Bold)) { append(name) }
            row.user?.alias?.let { withStyle(SpanStyle(color = Faint, fontSize = 10.sp)) { append(" $it") } }
            if (!row.action && row.parts.isNotEmpty()) withStyle(SpanStyle(color = AguColors.Muted)) { append(": ") } else append(" ")
        }
        row.rant?.let { r ->
            withStyle(SpanStyle(color = Color.White, fontWeight = FontWeight.Bold, fontSize = 11.sp, background = Color.Black.copy(alpha = 0.3f))) { append(" ${r.text} ") }
            append(" ")
        }
        val bodyStyle = when {
            grey -> SpanStyle(color = AguColors.Text.copy(alpha = 0.45f))
            row.action -> SpanStyle(color = nameColor, fontWeight = FontWeight.Medium)
            else -> SpanStyle()
        }
        withStyle(bodyStyle) { parts(b, row.parts, d) }
        // why a removed message went, after it (a greyed line only: the desktop's way)
        if (grey && !row.deletedNote.isNullOrEmpty()) {
            append(" ")
            withStyle(SpanStyle(color = Faint, fontStyle = FontStyle.Italic, fontSize = (d.bodySp * 0.9f).sp)) { append("—${row.deletedNote}") }
        }
    }
    return Built(text, b.inline)
}

/** A paint as a Compose brush over a name [w] x [h] px: the same gradient 7TV draws, with its angle and stops. */
private fun paintBrush(p: Paint, w: Float, h: Float): Brush? {
    if (p.stops.isEmpty()) return null
    val last = p.stops.maxOf { it.at }.takeIf { it > 0f } ?: 1f
    val stops = p.stops.sortedBy { it.at }.map { (if (p.repeat) it.at / last else it.at).coerceIn(0f, 1f) to (parseColor(it.color) ?: Color.White) }.toTypedArray()
    val tile = if (p.repeat) TileMode.Repeated else TileMode.Clamp
    if (p.function == "RADIAL_GRADIENT") return Brush.radialGradient(colorStops = stops, center = Offset(w / 2, h / 2), radius = max(w, h) / 2f, tileMode = tile)
    // CSS: 0deg points up, 90deg right; the gradient line spans |w sin a| + |h cos a|
    val a = Math.toRadians(p.angle.toDouble())
    val dx = sin(a).toFloat()
    val dy = -cos(a).toFloat()
    val len = abs(w * dx) + abs(h * dy)
    val c = Offset(w / 2, h / 2)
    return Brush.linearGradient(colorStops = stops, start = Offset(c.x - dx * len / 2, c.y - dy * len / 2), end = Offset(c.x + dx * len / 2, c.y + dy * len / 2), tileMode = tile)
}

/** An image of an emote or badge; still (its first frame) when animated emotes are off. */
@Composable
internal fun EmoteImage(url: String, animated: Boolean, modifier: Modifier = Modifier, onSuccess: ((Float) -> Unit)? = null, onError: (() -> Unit)? = null) {
    val ctx = LocalContext.current
    val req = remember(url, animated) {
        ImageRequest.Builder(ctx).data(url).apply {
            if (!animated) {
                decoderFactory(BitmapFactoryDecoder.Factory())
                memoryCacheKeyExtra("still", "1")
            }
        }.build()
    }
    AsyncImage(
        model = req, contentDescription = null, contentScale = ContentScale.Fit, modifier = modifier,
        onError = { onError?.invoke() },
        onSuccess = { st -> val img = st.result.image; if (onSuccess != null && img.height > 0) onSuccess(img.width.toFloat() / img.height) },
    )
}

/** Resolves the placeholders against the current aspect cache; reading it here subscribes the row to changes. */
@Composable
private fun inlineContent(spec: Map<String, Inl>, density: Density, d: Display, measurer: TextMeasurer, onTip: (Tip) -> Unit): Map<String, InlineTextContent> =
    spec.mapValues { (_, s) ->
        val bodyDp = d.bodySp
        when (s) {
            is Inl.Mark -> {
                val side = with(density) { MarkDp.dp.toSp() }
                InlineTextContent(Placeholder(side, side, PlaceholderVerticalAlign.TextCenter)) { PlatformMark(s.platform) }
            }
            is Inl.Img -> {
                val aspect = (aspects[s.url] ?: 1f).coerceIn(0.2f, 8f)
                val h = with(density) { s.heightDp.dp.toSp() }
                // a badge whose picture failed takes no room
                val w = if (s.badge && failedBadges[s.url] == true) with(density) { 1.dp.toSp() } else with(density) { (s.heightDp * aspect).dp.toSp() }
                InlineTextContent(Placeholder(w, h, PlaceholderVerticalAlign.TextCenter)) {
                    Box(Modifier.fillMaxSize().tipOnLongPress(s.tip, onTip)) {
                        EmoteImage(s.url, d.animatedEmotes, Modifier.fillMaxSize(), onSuccess = { aspects[s.url] = it; failedBadges.remove(s.url) }, onError = if (s.badge) ({ failedBadges[s.url] = true }) else null)
                        // Zero-width emotes sit on top of the base emote.
                        for (o in s.overlays) EmoteImage(o, d.animatedEmotes, Modifier.fillMaxSize())
                    }
                }
            }
            is Inl.Chip -> {
                val w = with(density) { (d.badgeDp * (if (s.glyph.length > 1) 1.7f else 1f)).dp.toSp() }
                val h = with(density) { d.badgeDp.dp.toSp() }
                InlineTextContent(Placeholder(w, h, PlaceholderVerticalAlign.TextCenter)) {
                    Box(
                        Modifier.fillMaxSize().clip(RoundedCornerShape(4.dp)).background(s.color.copy(alpha = 0.2f)).tipOnLongPress(s.tip, onTip),
                        contentAlignment = Alignment.Center,
                    ) { Text(s.glyph, color = s.color, fontSize = 10.sp, fontWeight = FontWeight.Bold, lineHeight = 10.sp, maxLines = 1) }
                }
            }
            is Inl.Emoji -> {
                val w = with(density) { (bodyDp * 1.25f).sp }
                val h = with(density) { d.emoteDp.dp.toSp() }
                InlineTextContent(Placeholder(w, h, PlaceholderVerticalAlign.TextCenter)) {
                    Box(Modifier.fillMaxSize().tipOnLongPress(s.tip, onTip), contentAlignment = Alignment.Center) {
                        Text(s.text, fontSize = (bodyDp * 1.1f).sp, maxLines = 1)
                        for (o in s.overlays) EmoteImage(o, d.animatedEmotes, Modifier.fillMaxSize())
                    }
                }
            }
            is Inl.Gif -> {
                val aspect = (aspects[s.url] ?: 1f).coerceIn(0.2f, 8f)
                val h = with(density) { GifDp.dp.toSp() }
                val w = with(density) { (GifDp * aspect).dp.toSp() }
                InlineTextContent(Placeholder(w, h, PlaceholderVerticalAlign.TextCenter)) {
                    // a tap opens the GIF's own link, as in Chatterino
                    val uri = LocalUriHandler.current
                    EmoteImage(s.url, d.animatedEmotes, Modifier.fillMaxSize().clickable { runCatching { uri.openUri(s.url) } }, onSuccess = { aspects[s.url] = it })
                }
            }
            is Inl.ModBtn -> {
                val wDp = if (s.b.label.isNotEmpty()) maxOf(ModBtnDp, s.b.label.length * 7f + 10f) else ModBtnDp
                InlineTextContent(Placeholder(with(density) { wDp.dp.toSp() }, with(density) { ModBtnDp.dp.toSp() }, PlaceholderVerticalAlign.TextCenter)) {
                    Box(Modifier.fillMaxSize().clip(RoundedCornerShape(5.dp)).clickable(onClick = s.onClick), contentAlignment = Alignment.Center) {
                        if (s.b.label.isNotEmpty()) Text(s.b.label, color = tone(s.b), fontSize = 10.sp, fontWeight = FontWeight.Bold, maxLines = 1, lineHeight = 10.sp)
                        else ModIcon(s.b.icon.ifEmpty { s.b.action }, tone(s.b), 17)
                    }
                }
            }
            is Inl.PaintName -> {
                val style = TextStyle(fontSize = bodyDp.sp, fontWeight = FontWeight.Bold)
                val size = measurer.measure(AnnotatedString(s.name), style, maxLines = 1).size
                val brush = paintBrush(s.paint, size.width.toFloat(), size.height.toFloat())
                val shadow = s.paint.shadows.firstOrNull()?.let { Shadow(parseColor(it.color) ?: Color.Black, Offset(it.x, it.y), it.radius) }
                val w = with(density) { size.width.toSp() }
                val h = with(density) { size.height.toSp() }
                InlineTextContent(Placeholder(w, h, PlaceholderVerticalAlign.TextCenter)) {
                    val plain = parseColor(s.paint.color) ?: s.base
                    androidx.compose.foundation.text.BasicText(
                        s.name, maxLines = 1, softWrap = false,
                        modifier = if (s.onClick != null) Modifier.clickable(onClick = s.onClick) else Modifier,
                        style = if (brush != null) style.copy(brush = brush, shadow = shadow) else style.copy(color = plain, shadow = shadow),
                    )
                }
            }
        }
    }

private fun Modifier.tipOnLongPress(tip: Tip?, onTip: (Tip) -> Unit): Modifier =
    if (tip == null) this else pointerInput(tip) { detectTapGestures(onLongPress = { onTip(tip) }) }

/** The popup of an emote, a badge or an emoji: the picture, its name and where it is from. */
@Composable
internal fun TipDialog(tip: Tip, d: Display, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = AguColors.Surface,
        confirmButton = { TextButton(onClick = onDismiss) { Text("Close", color = AguColors.Accent) } },
        text = {
            Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp)) {
                when {
                    tip.glyph != null -> Text(tip.glyph, fontSize = 56.sp)
                    !tip.img.isNullOrEmpty() -> EmoteImage(tip.img, d.animatedEmotes, Modifier.size(96.dp))
                }
                if (tip.name.isNotEmpty()) Text(tip.name, color = AguColors.Text, fontSize = 17.sp, fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center)
                if (tip.label.isNotEmpty()) Text(tip.label, color = AguColors.Muted, fontSize = 13.sp, textAlign = TextAlign.Center)
            }
        },
    )
}

private val ReplyBtn = Color(0x14FFFFFF)

/** How a message row is highlighted: a bar, a tint and a label, in the desktop's order. */
private class RowLook(val bar: Color?, val bg: Color?, val label: String? = null, val labelColor: Color = Color.Unspecified)

private fun lookOf(row: Row): RowLook {
    val rule = row.highlight?.let { h -> parseColor(h.color)?.let { it to h.alpha } }
    return when {
        row.mention -> RowLook(MentionRed, MentionRed.copy(alpha = 0.12f), "MENTIONS YOU", MentionLabel)
        rule != null -> RowLook(rule.first, rule.first.copy(alpha = rule.second))
        row.firstMsg -> RowLook(FirstPurple, FirstPurple.copy(alpha = 0.18f), "FIRST MESSAGE", FirstLabel)
        row.returning -> RowLook(ReturnBlue, ReturnBlue.copy(alpha = 0.08f), "RETURNING CHATTER", ReturnLabel)
        row.highlighted -> RowLook(null, EventPurple.copy(alpha = 0.10f))
        else -> RowLook(null, null)
    }
}

@Composable
fun MessageRow(
    row: Row,
    onLongPress: (Row) -> Unit,
    modifier: Modifier = Modifier,
    showMark: Boolean = false,
    flash: Boolean = false,
    onTap: (() -> Unit)? = null,
    /** a notice's Reply button: put @name in the message box */
    onReplyNotice: ((Row) -> Unit)? = null,
) {
    val d = LocalDisplay.current
    val longPress by rememberUpdatedState(onLongPress)
    var tip by remember { mutableStateOf<Tip?>(null) }
    val alt = d.alternateRows && row.ord % 2L == 1L
    val frame = modifier
        .fillMaxWidth()
        .pointerInput(row.key) { detectTapGestures(onTap = onTap?.let { t -> { _ -> t() } }, onLongPress = { longPress(row) }) }
        .let { if (alt) it.background(Color.White.copy(alpha = 0.03f)) else it }
        .let { if (flash) it.background(AmberBar.copy(alpha = 0.30f)) else it }
    val onTip: (Tip) -> Unit = { tip = it }
    // mod mode: the mod buttons stand at the start of the line
    val mod = LocalMod.current
    val modTab = LocalModTab.current
    val strip = mod != null && modTab != null && LocalModMode.current && row.mod != null && row.kind != "modfeed"
    if (strip && isPlainLine(row)) {
        val shown = d.modButtons.filter { allowed(it, row.mod!!) }
        val ctl = mod!!
        val vk = modTab!!.id
        val inline = remember(shown, row.key, ctl, vk) { InlineMod(shown) { b -> ctl.button(vk, row, b) } }
        androidx.compose.runtime.CompositionLocalProvider(LocalInlineMod provides inline) { RowContent(row, d, frame, showMark, onTip, onReplyNotice) }
    } else if (strip) LayoutRow(Modifier.fillMaxWidth()) {
        ModStrip(row, modTab!!.id, mod!!)
        RowContent(row, d, frame, showMark, onTip, onReplyNotice)
    } else RowContent(row, d, frame, showMark, onTip, onReplyNotice)
    tip?.let { TipDialog(it, d) { tip = null } }
}

/** A plain chat line (drawn by [LineRow]); its mod buttons go inline. */
private fun isPlainLine(row: Row) = row.kind != "modfeed" && row.kind != "automod" && row.notice?.seventv == null && row.kind != "notice" && row.event == null

/** One mod button's square, in dp (it sets the height of the line it sits on). */
private const val ModBtnDp = 26f

@Composable
private fun RowContent(row: Row, d: Display, frame: Modifier, showMark: Boolean, onTip: (Tip) -> Unit, onReplyNotice: ((Row) -> Unit)?) {
    when {
        row.kind == "modfeed" -> {
            val time = if (d.timestamps) formatTime(row.ts, d.timestampSeconds, is24Hour(LocalContext.current)) else null
            LayoutRow(frame.padding(horizontal = 8.dp, vertical = 1.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (time != null) Text(time, color = Faint, fontSize = (d.bodySp * 0.8f).sp, fontStyle = FontStyle.Italic)
                Text(row.system ?: row.plainText(), color = Color(0xFF64748B), fontSize = (d.bodySp * 0.9f).sp, fontStyle = FontStyle.Italic, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
        row.kind == "automod" -> AutomodRow(row, d, frame, onTip)
        row.notice?.seventv != null -> SevenNoticeRow(row, d, frame, onTip)
        row.event != null -> EventRow(row, d, frame, showMark, onTip)
        row.kind == "notice" -> NoticeRow(row, d, frame, showMark, onTip, onReplyNotice)
        else -> LineRow(row, d, frame, showMark, onTip)
    }
}

/** The message's text, with the card of its link under it. */
@Composable
private fun LineText(row: Row, d: Display, showMark: Boolean, onTip: (Tip) -> Unit, modifier: Modifier = Modifier) {
    val ctx = LocalContext.current
    val time = if (d.timestamps) formatTime(row.ts, d.timestampSeconds, is24Hour(ctx)) else null
    // tapping the name opens the user card
    val mod = LocalMod.current
    val inlineMod = LocalInlineMod.current
    val built = remember(row, d, showMark, time, mod, inlineMod?.buttons) { build(row, d, showMark, time, mod?.takeIf { row.user != null && row.kind != "modfeed" }?.let { c -> { c.openCard(row) } }, inlineMod) }
    val density = LocalDensity.current
    val measurer = rememberTextMeasurer()
    Column(modifier) {
        if (row.parts.isNotEmpty() || row.kind == "msg" && row.system.isNullOrEmpty() || row.user != null) {
            Text(built.text, inlineContent = inlineContent(built.inline, density, d, measurer, onTip), style = TextStyle(color = AguColors.Text, fontSize = d.bodySp.sp))
        }
        if (!row.deleted) row.link?.let { LinkCard(it, d) }
    }
}

@Composable
private fun LineRow(row: Row, d: Display, frame: Modifier, showMark: Boolean, onTip: (Tip) -> Unit) {
    val look = lookOf(row)
    var m = frame
    if (row.rant != null) {
        val c = parseColor(row.rant.color) ?: AguColors.Accent
        m = m.padding(vertical = 2.dp).clip(RoundedCornerShape(4.dp)).background(c.copy(alpha = 0.15f)).border(1.dp, c.copy(alpha = 0.5f), RoundedCornerShape(4.dp))
    } else if (look.bg != null) m = m.background(look.bg)
    val bar = look.bar
    if (bar != null) m = m.drawBehind { drawRect(bar, size = Size(3.dp.toPx(), size.height)) }
    Column(m.padding(start = if (bar != null) 11.dp else 8.dp, end = 8.dp, top = 3.dp, bottom = 3.dp)) {
        if (look.label != null) Text(look.label, color = look.labelColor, fontSize = 10.sp, fontWeight = FontWeight.Bold)
        row.reply?.let { r -> ReplyLine(row, r, d) }
        if (!row.system.isNullOrEmpty()) {
            Text(row.system, color = AguColors.Muted, fontSize = (d.bodySp * 0.8f).sp, fontWeight = FontWeight.Normal)
        }
        LineText(row, d, showMark, onTip)
    }
}

/** A redeem, a cheer or a KICKs gift: a purple bar and tint, who did what and for how much, then the message. */
@Composable
private fun EventRow(row: Row, d: Display, frame: Modifier, showMark: Boolean, onTip: (Tip) -> Unit) {
    val e = row.event ?: return
    val nameColor = parseColor(row.user?.color) ?: AguColors.Text
    val name = row.user?.name ?: row.user?.login.orEmpty()
    val look = lookOf(row)
    Column(frame.background(EventPurple.copy(alpha = 0.12f)).drawBehind { drawRect(EventPurple.copy(alpha = 0.9f), size = Size(3.dp.toPx(), size.height)) }.padding(start = 3.dp)) {
        EventHeader(name, nameColor, e, d)
        if (row.parts.isNotEmpty()) {
            var m = Modifier.fillMaxWidth()
            val bar = look.bar
            if (look.bg != null) m = m.background(look.bg)
            if (bar != null) m = m.drawBehind { drawRect(bar, size = Size(3.dp.toPx(), size.height)) }
            Column(m.padding(start = if (bar != null) 8.dp else 8.dp, end = 8.dp, top = 1.dp, bottom = 3.dp)) {
                row.reply?.let { r -> ReplyLine(row, r, d) }
                LineText(row, d, showMark, onTip)
            }
        }
    }
}

@Composable
private fun EventHeader(name: String, nameColor: Color, e: EventHead, d: Display) {
    LayoutRow(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 2.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            buildAnnotatedString {
                withStyle(SpanStyle(color = nameColor, fontWeight = FontWeight.SemiBold)) { append(name) }
                withStyle(SpanStyle(color = AguColors.Muted)) { append(" ${e.verb}") }
                if (e.kicks) {
                    append(" ")
                    withStyle(SpanStyle(color = KickGreen, fontWeight = FontWeight.SemiBold)) { append("Ⓚ ${e.amountText.orEmpty()}") }
                    e.reward?.let { withStyle(SpanStyle(color = AguColors.Muted)) { append(" · ") }; withStyle(SpanStyle(color = AguColors.Text, fontWeight = FontWeight.SemiBold)) { append(it) } }
                } else e.reward?.let { withStyle(SpanStyle(color = AguColors.Text, fontWeight = FontWeight.SemiBold)) { append(" $it") } }
            },
            fontSize = d.bodySp.sp, modifier = Modifier.weight(1f),
        )
        if (!e.kicks && e.amountText != null) {
            LayoutRow(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                val c = if (e.bits) parseColor(e.bitsColor) ?: AguColors.Muted else AccentSoft
                androidx.compose.foundation.Canvas(Modifier.size((d.bodySp * 0.95f).dp)) {
                    if (e.bits) {
                        // a bits gem
                        val w = size.width
                        val p = androidx.compose.ui.graphics.Path().apply {
                            moveTo(w * 0.5f, 0f); lineTo(w, w * 0.4f); lineTo(w * 0.5f, w); lineTo(0f, w * 0.4f); close()
                        }
                        drawPath(p, c)
                    } else drawCircle(c, radius = size.width * 0.42f, style = androidx.compose.ui.graphics.drawscope.Stroke(width = size.width * 0.16f))
                }
                Text(e.amountText, color = AguColors.Muted, fontSize = d.bodySp.sp, fontWeight = FontWeight.SemiBold)
            }
        }
        if (e.image != null) EmoteImage(e.image, d.animatedEmotes, Modifier.size((d.bodySp * 2.4f).dp))
    }
}

/** The app's own line about a live 7TV emote change: "A_g_u__ added the 7TV emote [picture] PotatWind". */
@Composable
private fun SevenNoticeRow(row: Row, d: Display, frame: Modifier, onTip: (Tip) -> Unit) {
    val n = row.notice?.seventv ?: return
    val ctx = LocalContext.current
    val time = if (d.timestamps) formatTime(row.ts, d.timestampSeconds, is24Hour(ctx)) else null
    val density = LocalDensity.current
    val measurer = rememberTextMeasurer()
    val inline = remember(n, d) { if (n.url.isNotEmpty()) mapOf("e" to Inl.Img(n.url, emptyList(), d.emoteDp, Tip(n.name, "7TV", n.tip.ifEmpty { n.url }))) else emptyMap() }
    val text = remember(n, d, time) {
        buildAnnotatedString {
            if (time != null) withStyle(SpanStyle(color = Faint, fontSize = (d.bodySp * 0.85f).sp)) { append(time); append(" ") }
            val bold = SpanStyle(color = AguColors.Text, fontWeight = FontWeight.SemiBold)
            val muted = SpanStyle(color = AguColors.Muted)
            if (!n.actor.isNullOrEmpty()) {
                withStyle(bold) { append(n.actor) }
                withStyle(muted) { append(" ${n.verb} ") }
            } else withStyle(muted) { append(n.verb.replaceFirstChar { it.uppercase() } + " ") }
            if (n.url.isNotEmpty()) { appendInlineContent("e", n.name); append(" ") }
            if (n.action == "renamed" && n.oldName.isNotEmpty()) { withStyle(bold) { append(n.oldName) }; withStyle(muted) { append(" to ") } }
            withStyle(bold) { append(n.name) }
        }
    }
    Box(frame.background(Color.White.copy(alpha = 0.035f)).drawBehind { drawRect(SlateBar.copy(alpha = 0.9f), size = Size(3.dp.toPx(), size.height)) }.padding(start = 11.dp, end = 8.dp, top = 4.dp, bottom = 4.dp)) {
        Text(text, inlineContent = inlineContent(inline, density, d, measurer, onTip), style = TextStyle(color = AguColors.Text, fontSize = d.bodySp.sp))
    }
}

/** A sub, a gift, a raid, an announcement, a Super Chat or a membership: a coloured bar, its icon, the words. */
@Composable
private fun NoticeRow(row: Row, d: Display, frame: Modifier, showMark: Boolean, onTip: (Tip) -> Unit, onReply: ((Row) -> Unit)?) {
    val n = row.notice
    val raid = n?.raid == true
    val sentence = n?.sentence ?: row.system.orEmpty()
    val barColor = parseColor(n?.bar) ?: when {
        raid -> RaidOrange.copy(alpha = 0.9f)
        n?.icon != null -> AguColors.Accent
        else -> EventPurple.copy(alpha = 0.9f)
    }
    val tint = when {
        n?.bar != null -> barColor.copy(alpha = barColor.alpha.coerceAtMost(0.5f) * 0.4f + 0.04f)
        n?.icon != null -> Color.White.copy(alpha = 0.035f)
        raid -> RaidOrange.copy(alpha = 0.12f)
        else -> EventPurple.copy(alpha = 0.12f)
    }
    val ctx = LocalContext.current
    Column(frame.background(tint).drawBehind { drawRect(barColor, size = Size(3.dp.toPx(), size.height)) }) {
        LayoutRow(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (n?.icon != null) Box(Modifier.padding(top = 2.dp).size(n.iconSize.dp)) { NoticeIcon(n.icon) }
            Column(Modifier.weight(1f)) {
                if (n?.announce == true) Text("Announcement", color = barColor.copy(alpha = 1f), fontSize = (d.bodySp * 0.75f).sp, fontWeight = FontWeight.Bold)
                val card = LocalMod.current
                n?.head?.let { Text(it, color = parseColor(n.headColor) ?: AguColors.Text, fontSize = d.bodySp.sp, fontWeight = FontWeight.Bold, modifier = if (card != null && row.user != null) Modifier.clickable { card.openCard(row) } else Modifier) }
                if (sentence.isNotEmpty()) {
                    val b = n?.bold
                    Text(
                        buildAnnotatedString {
                            if (n?.head == null && d.timestamps) withStyle(SpanStyle(color = Faint, fontSize = (d.bodySp * 0.85f).sp)) { append(formatTime(row.ts, d.timestampSeconds, is24Hour(ctx))); append(" ") }
                            if (b != null && b.size == 2 && b[1] > b[0] && b[1] <= sentence.length) {
                                append(sentence.substring(0, b[0]))
                                withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(sentence.substring(b[0], b[1])) }
                                append(sentence.substring(b[1]))
                            } else append(sentence)
                        },
                        color = if (raid) OrangeText else AguColors.Text, fontSize = d.bodySp.sp,
                        fontWeight = if (raid) FontWeight.Medium else FontWeight.Normal,
                    )
                }
                // a Kick raid's note
                if (raid && row.parts.isNotEmpty()) Text(row.plainText(), color = AguColors.Muted, fontSize = d.bodySp.sp)
                // the attached message is the sender's own chat line (a Super Chat's or a note's text too)
                if (n?.superChat == true && row.parts.isNotEmpty()) Text(row.plainText(), color = AguColors.Muted, fontSize = d.bodySp.sp)
                else if (!raid && row.parts.isNotEmpty() && row.user != null) {
                    LineText(noticeLine(row), d, showMark, onTip, Modifier.padding(top = 2.dp))
                }
                if (n?.reply == true && onReply != null) {
                    Box(
                        Modifier.padding(top = 4.dp).heightIn(min = 26.dp).clip(RoundedCornerShape(50)).background(ReplyBtn).clickable { onReply(row) }.padding(horizontal = 10.dp),
                        contentAlignment = Alignment.Center,
                    ) { Text("↩ Reply", color = AguColors.Text, fontSize = 11.sp) }
                }
            }
        }
    }
}

/** The chat line under a notice: the row itself read as a message (its own sentence is shown above). */
private fun noticeLine(row: Row): Row = row.copy(kind = "msg", system = null, notice = null, event = null, firstMsg = false, returning = false)

/** A small icon for a notice: a star (sub), a crown (Prime), a gift, a coin (Super Chat). */
@Composable
private fun NoticeIcon(icon: String) {
    androidx.compose.foundation.Canvas(Modifier.fillMaxSize()) {
        val w = size.width
        val c = AguColors.Text
        when (icon) {
            "star" -> {
                val p = androidx.compose.ui.graphics.Path()
                for (i in 0 until 10) {
                    val r = if (i % 2 == 0) w * 0.5f else w * 0.21f
                    val a = Math.toRadians(-90.0 + i * 36.0)
                    val x = w * 0.5f + (r * cos(a)).toFloat()
                    val y = w * 0.52f + (r * sin(a)).toFloat()
                    if (i == 0) p.moveTo(x, y) else p.lineTo(x, y)
                }
                p.close()
                drawPath(p, c)
            }
            "crown" -> {
                val p = androidx.compose.ui.graphics.Path().apply {
                    moveTo(w * 0.05f, w * 0.8f); lineTo(w * 0.05f, w * 0.25f); lineTo(w * 0.3f, w * 0.5f); lineTo(w * 0.5f, w * 0.15f)
                    lineTo(w * 0.7f, w * 0.5f); lineTo(w * 0.95f, w * 0.25f); lineTo(w * 0.95f, w * 0.8f); close()
                }
                drawPath(p, c)
            }
            "gift" -> {
                drawRect(c, Offset(w * 0.1f, w * 0.4f), Size(w * 0.8f, w * 0.5f))
                drawRect(c, Offset(w * 0.05f, w * 0.25f), Size(w * 0.9f, w * 0.18f))
                drawRect(Color(0xFF18181B), Offset(w * 0.45f, w * 0.25f), Size(w * 0.1f, w * 0.65f))
                drawCircle(c, w * 0.12f, Offset(w * 0.35f, w * 0.17f))
                drawCircle(c, w * 0.12f, Offset(w * 0.65f, w * 0.17f))
            }
            else -> {
                // coins
                drawCircle(c, radius = w * 0.36f, center = Offset(w * 0.4f, w * 0.6f))
                drawCircle(c.copy(alpha = 0.7f), radius = w * 0.36f, center = Offset(w * 0.62f, w * 0.4f))
            }
        }
    }
}

/** The AutoMod card: what was held and why, the held message, and what happened to it. */
@Composable
private fun AutomodRow(row: Row, d: Display, frame: Modifier, onTip: (Tip) -> Unit) {
    Column(frame.background(AmberBar.copy(alpha = 0.08f)).drawBehind { drawRect(AmberBar, size = Size(3.dp.toPx(), size.height)) }.padding(start = 11.dp, end = 8.dp, top = 4.dp, bottom = 4.dp)) {
        LayoutRow(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            Text("AutoMod held a message", color = AmberText, fontSize = (d.bodySp * 0.92f).sp, fontWeight = FontWeight.Bold)
            row.chip?.let {
                Text(
                    it, color = Color(0xFFFCD34D), fontSize = 10.sp,
                    modifier = Modifier.clip(RoundedCornerShape(50)).background(AmberBar.copy(alpha = 0.1f)).border(1.dp, AmberBar.copy(alpha = 0.3f), RoundedCornerShape(50)).padding(horizontal = 6.dp, vertical = 1.dp),
                )
            }
        }
        LineText(row.copy(kind = "msg", system = null), d, false, onTip, Modifier.padding(top = 2.dp))
        row.resolved?.let { Text(it, color = Faint, fontSize = 11.sp, modifier = Modifier.padding(top = 2.dp)) }
        // Allow / Deny while it is held, for an account that may resolve holds
        val mod = LocalMod.current
        val id = row.id?.content
        if (mod != null && id != null && row.canResolve && row.status == "held") LayoutRow(Modifier.padding(top = 6.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Pill("Allow", Color(0xFF86EFAC)) { mod.automod(row.sk, id, true) }
            Pill("Deny", Color(0xFFFCA5A5)) { mod.automod(row.sk, id, false) }
        }
    }
}

/** The card under a message that links to a Twitch clip, a YouTube video, a post on X, Streamable or Instagram. */
@Composable
private fun LinkCard(link: LinkRef, d: Display) {
    val store = LocalPreviews.current ?: return
    if (!d.linkPreviews) return
    LaunchedEffect(link.key) { store.request(link) }
    val p = store.cards[link.key] ?: return
    val uri = LocalUriHandler.current
    val titleSp = (d.bodySp * 0.95f)
    LayoutRow(
        Modifier.padding(top = 3.dp, bottom = 2.dp).widthIn(max = 440.dp).fillMaxWidth()
            .clip(RoundedCornerShape(6.dp)).background(Color(0xFF1B1B1F)).border(1.dp, Color(0xFF2E2E33), RoundedCornerShape(6.dp))
            .clickable { runCatching { uri.openUri(link.href) } }.padding(5.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        if (p.thumb != null) {
            AsyncImage(model = p.thumb, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.size(89.dp, 50.dp).clip(RoundedCornerShape(3.dp)).background(AguColors.Surface2))
        } else {
            Box(Modifier.size(20.dp), contentAlignment = Alignment.Center) { Text(p.provider.take(1), color = AguColors.Text, fontSize = 12.sp, fontWeight = FontWeight.Bold) }
        }
        Column(Modifier.weight(1f)) {
            Text(p.title, color = AguColors.Text, fontSize = titleSp.sp, fontWeight = FontWeight.SemiBold, maxLines = 2, overflow = TextOverflow.Ellipsis)
            if (p.subtitle.isNotEmpty()) Text(p.subtitle, color = AguColors.Muted, fontSize = (d.bodySp * 0.85f).sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
    }
}

/** The small platform-coloured mark in front of a line of a merged tab. */
@Composable
fun PlatformMark(platform: String, modifier: Modifier = Modifier) {
    val v = platformVector(platform)
    if (v != null) androidx.compose.material3.Icon(v, contentDescription = platformLabel(platform), tint = platformColor(platform), modifier = modifier.fillMaxSize())
    else Box(modifier.fillMaxSize().clip(RoundedCornerShape(3.dp)).background(platformColor(platform)))
}
