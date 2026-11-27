package lol.agu.chat.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.unit.dp

/** Said under the username box when whispering (the desktop's words). */
const val WHISPER_HINT = "Twitch username to whisper. Needs a Twitch login with whisper access, and a verified phone number on your Twitch account."

/** A whisper conversation's mark: an envelope in Twitch purple, where other chats show their platform's mark. */
@Composable
fun MailIcon(sizeDp: Int = 12, color: Color = platformColor("twitch"), modifier: Modifier = Modifier) {
    Canvas(modifier.size(sizeDp.dp)) {
        val w = size.width
        val st = Stroke(width = w * 0.11f, cap = StrokeCap.Round, join = StrokeJoin.Round)
        drawRoundRect(color, topLeft = Offset(w * 0.08f, w * 0.2f), size = Size(w * 0.84f, w * 0.6f), cornerRadius = CornerRadius(w * 0.1f), style = st)
        val flap = Path().apply { moveTo(w * 0.1f, w * 0.26f); lineTo(w * 0.5f, w * 0.58f); lineTo(w * 0.9f, w * 0.26f) }
        drawPath(flap, color, style = st)
    }
}
