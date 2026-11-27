package lol.agu.chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** The message box with an optional reply strip; read-only chats show why and a way to log in. */
@Composable
fun Composer(
    text: String,
    onText: (String) -> Unit,
    canSend: Boolean,
    readOnlyReason: String?,
    replyingTo: String?,
    onCancelReply: () -> Unit,
    onSend: () -> Unit,
    onLogin: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier.fillMaxWidth().background(AguColors.Bg)) {
        if (replyingTo != null) {
            Row(
                Modifier.fillMaxWidth().background(AguColors.Surface).padding(horizontal = 12.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("Replying to @$replyingTo", color = AguColors.Muted, fontSize = 13.sp, modifier = Modifier.weight(1f))
                Text(
                    "×", color = AguColors.Text, fontSize = 22.sp,
                    modifier = Modifier.clip(CircleShape).clickable(onClick = onCancelReply).padding(horizontal = 10.dp),
                )
            }
        }
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 10.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Box(
                Modifier.weight(1f).heightIn(min = 44.dp).clip(RoundedCornerShape(22.dp)).background(AguColors.Surface2)
                    .padding(horizontal = 16.dp, vertical = 10.dp),
                contentAlignment = Alignment.CenterStart,
            ) {
                if (!canSend) {
                    Text(readOnlyReason?.takeIf { it.isNotBlank() } ?: "Log in to send", color = AguColors.Muted, fontSize = 15.sp, maxLines = 1)
                } else {
                    if (text.isEmpty()) Text("Send a message", color = AguColors.Muted, fontSize = 15.sp)
                    BasicTextField(
                        value = text,
                        onValueChange = onText,
                        textStyle = TextStyle(color = AguColors.Text, fontSize = 15.sp),
                        cursorBrush = SolidColor(AguColors.Accent),
                        maxLines = 4,
                        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                        keyboardActions = KeyboardActions(onSend = { onSend() }),
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
            if (canSend) {
                val enabled = text.isNotBlank()
                Box(
                    Modifier.size(44.dp).clip(CircleShape)
                        .background(if (enabled) AguColors.Accent else AguColors.Surface2)
                        .clickable(enabled = enabled, onClick = onSend),
                    contentAlignment = Alignment.Center,
                ) {
                    Text("↑", color = if (enabled) androidx.compose.ui.graphics.Color.White else AguColors.Muted, fontSize = 22.sp, textAlign = TextAlign.Center)
                }
            } else {
                TextButton(onClick = onLogin) { Text("Log in", color = AguColors.Accent) }
            }
        }
    }
}
