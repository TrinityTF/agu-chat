package lol.agu.chat.ui

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
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.launch
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

private val ERROR_RED = Color(0xFFFF6B6B)

// the desktop's order (Settings -> Accounts)
private val order = listOf("twitch", "rumble", "kick", "x", "youtube")

@Composable
fun AccountsScreen(vm: ChatViewModel, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    Column(Modifier.fillMaxSize().background(AguColors.Bg).statusBarsPadding().navigationBarsPadding().imePadding()) {
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
            for (p in order) AccountCard(p, vm.accounts[p] ?: AccountState(), vm)
        }
        Text(
            "Agu Chat ${vm.appVersion}", color = AguColors.Muted, fontSize = 12.sp,
            modifier = Modifier.fillMaxWidth().padding(12.dp), textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        )
    }
}

/** A login that ended because the platform stopped accepting it says so in its error; the button then says "Log in again". */
private fun needsRelogin(state: AccountState): Boolean {
    val e = state.error?.lowercase() ?: return false
    return !state.connected && (e.contains("expired") || e.contains("log in again") || e.contains("signed you out"))
}

@Composable
private fun AccountCard(platform: String, state: AccountState, vm: ChatViewModel) {
    val scope = rememberCoroutineScope()
    val ctx = LocalContext.current
    val err = vm.loginErrors[platform] ?: state.error
    val pending = state.pending
    Column(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp)).background(AguColors.Surface).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            PlatformLogo(platform, 18.dp)
            Text(platformLabel(platform), color = AguColors.Text, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
        }
        val loginLabel = if (needsRelogin(state)) "Log in again" else "Log in"
        when {
            state.unsupported ->
                Muted("Read-only in this beta. You can read chat, but sending and moderating from here is not available yet.")
            state.connected -> ConnectedLine(state) { vm.logout(platform) }
            pending != null -> when (platform) {
                "twitch" -> Waiting("Waiting for you in the Twitch window...", vm, platform)
                "kick" -> {
                    Muted("Waiting for you to approve in your browser…")
                    Row {
                        TextButton(onClick = { openUrl(ctx, pending.verificationUri) }) { Text("Open browser again", color = AguColors.Accent) }
                        TextButton(onClick = { vm.cancelLogin(platform) }) { Text("Cancel", color = AguColors.Muted) }
                    }
                }
                "x" -> Waiting("Waiting for you to log in to X in the sign-in window…", vm, platform)
                "rumble" -> Waiting("Waiting for you to log in to Rumble in the sign-in window…", vm, platform)
                else -> Waiting("Waiting for you to log in to YouTube in the sign-in window…", vm, platform)
            }
            else -> {
                PrimaryButton(if (platform == "twitch" && loginLabel == "Log in") "Log in with Twitch" else loginLabel) {
                    scope.launch { vm.login(platform) }
                }
                when (platform) {
                    "kick" -> Hint("You approve it on kick.com in your browser; your password never goes through Agu Chat.")
                    "x" -> Hint("X chat shows names and can be written to once you log in. You sign in on x.com inside Agu Chat; only the session is kept, encrypted on this phone.")
                    "rumble" -> Hint("Log in to write in Rumble chat and moderate it. You sign in on Rumble's own page; only the session is kept, encrypted on this phone.")
                    "youtube" -> Hint("Log in to write in YouTube chat and moderate it. You sign in on Google inside Agu Chat; only the session is kept, encrypted on this phone.")
                }
            }
        }
        err?.takeIf { it.isNotBlank() }?.let { Text(it, color = ERROR_RED, fontSize = 13.sp) }
    }
}

@Composable
private fun Muted(text: String) = Text(text, color = AguColors.Muted, fontSize = 14.sp)

@Composable
private fun Hint(text: String) = Text(text, color = AguColors.Muted, fontSize = 12.sp)

@Composable
private fun PrimaryButton(text: String, enabled: Boolean = true, onClick: () -> Unit) = Button(
    onClick = onClick, enabled = enabled,
    colors = ButtonDefaults.buttonColors(containerColor = AguColors.Accent),
) { Text(text) }

@Composable
private fun ConnectedLine(state: AccountState, onLogout: () -> Unit) {
    val name = state.user?.name?.takeIf { it.isNotBlank() } ?: state.user?.login?.takeIf { it.isNotBlank() } ?: "unknown"
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text("Logged in as $name", color = AguColors.Text, fontSize = 14.sp, modifier = Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
        OutlinedButton(onClick = onLogout) { Text("Log out", color = AguColors.Text) }
    }
}

@Composable
private fun Waiting(text: String, vm: ChatViewModel, platform: String) {
    Muted(text)
    TextButton(onClick = { vm.cancelLogin(platform) }) { Text("Cancel", color = AguColors.Muted) }
}

private fun openUrl(ctx: Context, url: String) {
    runCatching { ctx.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)) }
}
