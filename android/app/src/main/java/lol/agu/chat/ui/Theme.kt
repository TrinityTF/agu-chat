package lol.agu.chat.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color

object AguColors {
    val Bg = Color(0xFF0E0E10)
    val Surface = Color(0xFF18181B)
    val Surface2 = Color(0xFF1F1F23)
    val Text = Color(0xFFEFEFF1)
    val Muted = Color(0xFFADADB8)
    val Accent = Color(0xFFD946EF)
    val Purple = Color(0xFFA855F7)
    val Indigo = Color(0xFF6366F1)
    val Amber = Color(0xFFF59E0B)
    val Danger = Color(0xFFEF4444)
    val Green = Color(0xFF22C55E)

    /** The brand gradient (purple, fuchsia, indigo), diagonal. */
    val Brand = Brush.linearGradient(listOf(Purple, Accent, Indigo))
}

/** Platform ids as the core knows them, in display order. */
val platforms = listOf("twitch", "kick", "youtube", "rumble", "x")

fun platformLabel(p: String) = when (p) {
    "twitch" -> "Twitch"
    "kick" -> "Kick"
    "youtube" -> "YouTube"
    "rumble" -> "Rumble"
    "x" -> "X"
    else -> p
}

fun platformColor(p: String) = when (p) {
    "twitch" -> Color(0xFF9146FF)
    "kick" -> Color(0xFF53FC18)
    "youtube" -> Color(0xFFFF0000)
    "rumble" -> Color(0xFF85C742)
    "x" -> Color(0xFFFFFFFF)
    else -> AguColors.Muted
}

@Composable
fun AguTheme(content: @Composable () -> Unit) {
    @Suppress("UNUSED_VARIABLE") val dark = isSystemInDarkTheme() // the app is dark-only
    MaterialTheme(
        colorScheme = darkColorScheme(
            primary = AguColors.Accent,
            onPrimary = Color.White,
            background = AguColors.Bg,
            onBackground = AguColors.Text,
            surface = AguColors.Surface,
            onSurface = AguColors.Text,
            surfaceContainer = AguColors.Surface,
            surfaceContainerHigh = AguColors.Surface,
            surfaceVariant = AguColors.Surface2,
            onSurfaceVariant = AguColors.Muted,
            error = AguColors.Danger,
        ),
        content = content,
    )
}
