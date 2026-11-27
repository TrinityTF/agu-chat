package lol.agu.chat.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Icon
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.PathParser
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

// The platform logos of the desktop app (native/gpui/assets/icons/platform-*.svg), built in code so no resource
// lookup can lose them in a shrunk release build.
private class Logo(val w: Float, val h: Float, val d: String)

private val logos = mapOf(
    "twitch" to Logo(24f, 24f, "M4.3 1 1.7 4.5v15.2h5.2V23h3l3.3-3.3h4.4L23 14.4V1H4.3zm16.6 12.5-3 3h-4.8l-3.2 3.2v-3.2H5.7V3.1h15.2v10.4zM17 6.5h-2.1v5.2H17V6.5zm-5.6 0H9.3v5.2h2.1V6.5z"),
    "kick" to Logo(24f, 24f, "M1.333 0h8v5.333H12V2.667h2.667V0h8v8H20v2.667h-2.667v2.666H20V16h2.667v8h-8v-2.667H12v-2.666H9.333V24h-8Z"),
    "youtube" to Logo(24f, 24f, "M23.5 6.2a3 3 0 0 0-2.1-2.1C19.5 3.6 12 3.6 12 3.6s-7.5 0-9.4.5A3 3 0 0 0 .5 6.2C0 8.1 0 12 0 12s0 3.9.5 5.8a3 3 0 0 0 2.1 2.1c1.9.5 9.4.5 9.4.5s7.5 0 9.4-.5a3 3 0 0 0 2.1-2.1c.5-1.9.5-5.8.5-5.8s0-3.9-.5-5.8zM9.5 15.6V8.4l6.3 3.6-6.3 3.6z"),
    "rumble" to Logo(32f, 34f, "M19.1 19a2.5 2.5 0 0 0 0-4c-2-1.5-4-2.8-6.2-3.7-1.5-.7-3.2.2-3.4 1.9-.4 2.5-.5 5.1-.2 7.5a2.4 2.4 0 0 0 3.4 2 28.3 28.3 0 0 0 6.4-3.6Zm10-7.3a7.8 7.8 0 0 1 0 10.8c-5.2 5.3-11.6 9-18.8 10.7a7.3 7.3 0 0 1-8.8-5A39.6 39.6 0 0 1 1.7 6 7.3 7.3 0 0 1 10.3.7c7 1.6 13.7 5.7 18.7 11Z"),
    "x" to Logo(24f, 24f, "M18.244 2.25h3.308l-7.227 8.26 8.502 11.24H16.17l-5.214-6.817L4.99 21.75H1.68l7.73-8.835L1.254 2.25H8.08l4.713 6.231zm-1.161 17.52h1.833L7.084 4.126H5.117z"),
)

private val vectors = HashMap<String, ImageVector>()

internal fun platformVector(platform: String): ImageVector? = vectorFor(platform)

private fun vectorFor(platform: String): ImageVector? {
    vectors[platform]?.let { return it }
    val l = logos[platform] ?: return null
    val v = ImageVector.Builder(name = platform, defaultWidth = 24.dp, defaultHeight = (24f * l.h / l.w).dp, viewportWidth = l.w, viewportHeight = l.h)
        .addPath(pathData = PathParser().parsePathString(l.d).toNodes(), fill = SolidColor(Color.White))
        .build()
    vectors[platform] = v
    return v
}

/** A platform's logo in its colour (X in white), [size] square; a dot for a platform without one. */
@Composable
fun PlatformLogo(platform: String, size: Dp, modifier: Modifier = Modifier, tint: Color = platformColor(platform)) {
    Box(modifier.size(size), contentAlignment = Alignment.Center) {
        val v = vectorFor(platform)
        if (v != null) Icon(v, contentDescription = platformLabel(platform), tint = tint, modifier = Modifier.size(size))
        else Box(Modifier.size(size / 2))
    }
}
