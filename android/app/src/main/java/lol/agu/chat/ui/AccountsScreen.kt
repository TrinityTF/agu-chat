package lol.agu.chat.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

@Composable
fun AccountsScreen(vm: ChatViewModel, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    Column(Modifier.fillMaxSize().background(AguColors.Bg).statusBarsPadding().navigationBarsPadding()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                "←", color = AguColors.Text, fontSize = 24.sp,
                modifier = Modifier.clip(CircleShape).clickable(onClick = onBack).padding(horizontal = 14.dp, vertical = 6.dp),
            )
            Text("Accounts", color = AguColors.Text, fontSize = 20.sp, fontWeight = FontWeight.Bold)
        }
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            for (p in platforms) AccountCard(p, vm.accounts[p] ?: AccountState(), vm)
        }
        Text(
            "Agu Chat ${vm.appVersion}", color = AguColors.Muted, fontSize = 12.sp,
            modifier = Modifier.fillMaxWidth().padding(12.dp), textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        )
    }
}

@Composable
private fun AccountCard(platform: String, state: AccountState, vm: ChatViewModel) {
    val ctx = LocalContext.current
    // Only Twitch and Kick can log in on Android for now.
    val comingSoon = platform !in listOf("twitch", "kick") || state.unsupported
    Column(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp)).background(AguColors.Surface).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Box(Modifier.size(10.dp).clip(CircleShape).background(platformColor(platform)))
            Text(platformLabel(platform), color = AguColors.Text, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
        }
        when {
            comingSoon -> Text("Coming soon on Android", color = AguColors.Muted, fontSize = 14.sp)
            state.connected -> {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        "Logged in as ${state.user?.name ?: state.user?.login ?: "?"}",
                        color = AguColors.Text, fontSize = 14.sp, modifier = Modifier.weight(1f),
                    )
                    OutlinedButton(onClick = { vm.accountCall("accounts.logout", platform) }) { Text("Log out", color = AguColors.Text) }
                }
            }
            state.pending != null -> {
                val pending = state.pending
                Text("Enter this code", color = AguColors.Muted, fontSize = 14.sp)
                Text(
                    pending.userCode, color = AguColors.Text, fontSize = 34.sp,
                    fontFamily = FontFamily.Monospace, fontWeight = FontWeight.Bold, letterSpacing = 4.sp,
                )
                Button(
                    onClick = { openUrl(ctx, pending.verificationUri) },
                    colors = ButtonDefaults.buttonColors(containerColor = AguColors.Accent),
                ) { Text("Open twitch.tv/activate") }
                Row {
                    TextButton(onClick = { copy(ctx, pending.userCode) }) { Text("Copy code", color = AguColors.Accent) }
                    TextButton(onClick = { vm.accountCall("accounts.cancel", platform) }) { Text("Cancel", color = AguColors.Muted) }
                }
            }
            else -> Button(
                onClick = { vm.accountCall("accounts.login", platform) },
                colors = ButtonDefaults.buttonColors(containerColor = AguColors.Accent),
            ) { Text("Log in") }
        }
        state.error?.takeIf { it.isNotBlank() }?.let { Text(it, color = Color(0xFFFF6B6B), fontSize = 13.sp) }
    }
}

private fun openUrl(ctx: Context, url: String) {
    runCatching { ctx.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)) }
}

private fun copy(ctx: Context, text: String) {
    (ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("code", text))
}
