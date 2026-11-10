package com.airwave.client

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import androidx.core.content.IntentCompat
import kotlinx.coroutines.delay

// Colours shared with the computer app.
private val BgMain = Color(0xFF161920)
private val BgBar = Color(0xFF101218)
private val Card = Color(0xFF1D212A)
private val CardHi = Color(0xFF252A35)
private val Border = Color(0xFF2A303C)
private val TextMain = Color(0xFFE9ECF2)
private val Muted = Color(0xFF8B93A4)
private val Faint = Color(0xFF5B6375)
private val Accent = Color(0xFF47D1B8)
private val AccentDark = Color(0xFF163A36)
private val OnAccent = Color(0xFF08201C)
private val Danger = Color(0xFFF06B6B)
private val Warn = Color(0xFFF2C14E)

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Airwave.load(this)
        handleAccessory(intent)
        setContent { AirwaveApp() }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleAccessory(intent)
    }

    override fun onResume() {
        super.onResume()
        // Already plugged in before the app was opened?
        if (!Airwave.accessoryAttached) {
            val manager = getSystemService(Context.USB_SERVICE) as UsbManager
            manager.accessoryList?.firstOrNull { it.manufacturer == "Airwave" }?.let { acc ->
                if (manager.hasPermission(acc)) Airwave.listenToAccessory(this, acc)
            }
        }
    }

    private fun handleAccessory(intent: Intent?) {
        if (intent?.action != UsbManager.ACTION_USB_ACCESSORY_ATTACHED) return
        val acc = IntentCompat.getParcelableExtra(intent, UsbManager.EXTRA_ACCESSORY, UsbAccessory::class.java)
            ?: return
        Airwave.listenToAccessory(this, acc)
    }
}

private enum class Tab(val title: String, val glyph: String) {
    Listen("Listen", "♪"),
    Send("Send", "◉"),
    Usb("USB", "⇄"),
}

@Composable
fun AirwaveApp() {
    val colors = darkColorScheme(
        primary = Accent,
        onPrimary = OnAccent,
        background = BgMain,
        surface = Card,
        onSurface = TextMain,
        onBackground = TextMain,
        error = Danger,
    )
    var tab by rememberSaveable { mutableIntStateOf(0) }
    val context = LocalContext.current

    // Look for computers every few seconds.
    LaunchedEffect(Unit) {
        while (true) {
            Airwave.search(context)
            delay(4000)
        }
    }

    MaterialTheme(colorScheme = colors) {
        Scaffold(
            containerColor = BgMain,
            topBar = { TopBar() },
            bottomBar = {
                NavigationBar(containerColor = BgBar) {
                    Tab.entries.forEachIndexed { i, t ->
                        NavigationBarItem(
                            selected = tab == i,
                            onClick = { tab = i },
                            icon = { Text(t.glyph, fontSize = 18.sp) },
                            label = { Text(t.title) },
                            colors = NavigationBarItemDefaults.colors(
                                selectedIconColor = Accent,
                                selectedTextColor = TextMain,
                                indicatorColor = AccentDark,
                                unselectedIconColor = Muted,
                                unselectedTextColor = Muted,
                            ),
                        )
                    }
                }
            },
        ) { pad ->
            Column(
                Modifier
                    .padding(pad)
                    .fillMaxSize()
                    .verticalScroll(rememberScrollState())
                    .padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                when (Tab.entries[tab]) {
                    Tab.Listen -> ListenScreen()
                    Tab.Send -> SendScreen()
                    Tab.Usb -> UsbScreen()
                }
            }
        }
    }
}

@Composable
private fun TopBar() {
    Row(
        Modifier
            .fillMaxWidth()
            .background(BgBar)
            .statusBarsPadding()
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Logo()
        Column {
            Text("Airwave", color = TextMain, fontSize = 20.sp, fontWeight = FontWeight.Bold)
            Text("Sound across your network", color = Muted, fontSize = 12.sp)
        }
    }
}

@Composable
private fun Logo() {
    Row(
        Modifier
            .size(40.dp)
            .clip(RoundedCornerShape(11.dp))
            .background(AccentDark),
        horizontalArrangement = Arrangement.spacedBy(2.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        listOf(10, 18, 26, 16, 22, 12).forEach { h ->
            Box(
                Modifier
                    .width(3.dp)
                    .height(h.dp)
                    .clip(CircleShape)
                    .background(Accent)
            )
        }
    }
}

// ----------------------------------------------------------------- pieces

@Composable
private fun CardBox(content: @Composable () -> Unit) {
    Column(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(Card)
            .border(1.dp, Border, RoundedCornerShape(14.dp))
            .padding(18.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) { content() }
}

@Composable
private fun CardTitle(title: String, hint: String = "") {
    Column {
        Text(title, color = TextMain, fontWeight = FontWeight.SemiBold, fontSize = 15.sp)
        if (hint.isNotEmpty()) Text(hint, color = Muted, fontSize = 12.sp)
    }
}

@Composable
private fun Dot(color: Color) {
    Box(
        Modifier
            .size(10.dp)
            .clip(CircleShape)
            .background(color)
    )
}

@Composable
private fun Status(color: Color, title: String, sub: String, subColor: Color = Muted) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Dot(color)
            Text(title, color = TextMain, fontSize = 18.sp, fontWeight = FontWeight.SemiBold)
        }
        if (sub.isNotEmpty()) Text(sub, color = subColor, fontSize = 13.sp)
    }
}

@Composable
private fun Meter(level: Float, active: Boolean) {
    val n = 24
    val pos = if (level <= 0.000001f) 0f else ((20f * kotlin.math.log10(level) + 60f) / 60f).coerceIn(0f, 1f)
    val lit = (pos * n).toInt()
    Row(horizontalArrangement = Arrangement.spacedBy(3.dp), verticalAlignment = Alignment.Bottom) {
        for (i in 0 until n) {
            val color = when {
                !active || i >= lit -> Border
                i >= n - 2 -> Danger
                i >= n - 7 -> Warn
                else -> Accent
            }
            Box(
                Modifier
                    .width(6.dp)
                    .height((8 + 12 * i / n).dp)
                    .clip(RoundedCornerShape(2.dp))
                    .background(color)
            )
        }
    }
}

@Composable
private fun PrimaryButton(text: String, onClick: () -> Unit) {
    Button(
        onClick = onClick,
        shape = RoundedCornerShape(10.dp),
        colors = ButtonDefaults.buttonColors(containerColor = Accent, contentColor = OnAccent),
    ) { Text(text, fontWeight = FontWeight.SemiBold) }
}

@Composable
private fun QuietButton(text: String, color: Color = TextMain, onClick: () -> Unit) {
    OutlinedButton(
        onClick = onClick,
        shape = RoundedCornerShape(10.dp),
        colors = ButtonDefaults.outlinedButtonColors(containerColor = CardHi, contentColor = color),
    ) { Text(text, fontWeight = FontWeight.SemiBold) }
}

@Composable
private fun Chip(text: String, selected: Boolean, onClick: () -> Unit) {
    FilterChip(
        selected = selected,
        onClick = onClick,
        label = { Text(text) },
        colors = FilterChipDefaults.filterChipColors(
            selectedContainerColor = AccentDark,
            selectedLabelColor = Accent,
            labelColor = Muted,
        ),
    )
}

// ---------------------------------------------------------------- screens

@Composable
private fun ListenScreen() {
    val context = LocalContext.current
    val state = Airwave.player
    Header("Listen", "Play the sound your computer sends.")

    CardBox {
        when (state) {
            PlayerState.Idle -> Status(Faint, "Not playing", "Pick a computer below.")
            is PlayerState.Connecting -> Status(Warn, "Connecting…", "Reaching ${state.from}")
            is PlayerState.Playing -> Status(Accent, "Playing from ${state.from}", state.format)
            is PlayerState.Failed -> Status(Danger, "Stopped", state.message, Danger)
        }
        Meter(Airwave.playerLevel, state is PlayerState.Playing)
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Text("Volume", color = Muted, fontSize = 12.sp)
            Slider(
                value = Airwave.volume,
                onValueChange = { Airwave.volume = it; Airwave.muted = false },
                onValueChangeFinished = { Airwave.save(context) },
                modifier = Modifier.weight(1f),
                colors = SliderDefaults.colors(thumbColor = Accent, activeTrackColor = Accent, inactiveTrackColor = Border),
            )
            Text(if (Airwave.muted) "Muted" else "${(Airwave.volume * 100).toInt()}%", color = TextMain, fontSize = 13.sp)
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            QuietButton(if (Airwave.muted) "Unmute" else "Mute") { Airwave.muted = !Airwave.muted }
            if (Airwave.isPlaying) QuietButton("Disconnect", Danger) { Airwave.stopPlayer(context) }
        }
    }

    CardBox {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.weight(1f)) { CardTitle("Computers nearby", "Running Airwave and sending") }
            QuietButton(if (Airwave.searching) "Searching…" else "Search") { Airwave.search(context) }
        }
        if (Airwave.servers.isEmpty()) {
            Text("None found yet. Press Start sending on the computer.", color = Muted, fontSize = 13.sp)
        }
        Airwave.servers.forEach { f ->
            Row(
                Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(10.dp))
                    .background(BgMain)
                    .padding(horizontal = 12.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Box(
                    Modifier
                        .size(32.dp)
                        .clip(RoundedCornerShape(8.dp))
                        .background(AccentDark),
                    contentAlignment = Alignment.Center,
                ) { Text(f.name.take(1).uppercase(), color = Accent, fontWeight = FontWeight.Bold) }
                Column(Modifier.weight(1f)) {
                    Text(f.name, color = TextMain, fontSize = 14.sp)
                    Text(f.address, color = Muted, fontSize = 12.sp)
                }
                PrimaryButton("Listen") { Airwave.listenTo(context, f.address) }
            }
        }
    }

    CardBox {
        CardTitle("Connect by address", "If your computer isn't listed, type the address it shows")
        var address by rememberSaveable { mutableStateOf("") }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            OutlinedTextField(
                value = address,
                onValueChange = { address = it.filter { c -> c.isLetterOrDigit() || c in ".:-" }.take(64) },
                placeholder = { Text("e.g. 192.168.1.20") },
                singleLine = true,
                modifier = Modifier.weight(1f),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Go),
                keyboardActions = KeyboardActions(onGo = { Airwave.listenTo(context, address) }),
                colors = OutlinedTextFieldDefaults.colors(
                    focusedBorderColor = Accent,
                    unfocusedBorderColor = Border,
                    focusedTextColor = TextMain,
                    unfocusedTextColor = TextMain,
                ),
            )
            PrimaryButton("Connect") { Airwave.listenTo(context, address) }
        }
    }

    CardBox {
        CardTitle("Buffer", "How much sound waits before playing. Lower = less delay, higher = fewer dropouts")
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            listOf(40 to "Low", 100 to "Balanced", 250 to "Stable").forEach { (ms, name) ->
                Chip(name, Airwave.bufferMs == ms) {
                    Airwave.bufferMs = ms
                    Airwave.save(context)
                    Airwave.restartIfPlaying(context)
                }
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Slider(
                value = Airwave.bufferMs.toFloat(),
                onValueChange = { Airwave.bufferMs = (it / 10f).toInt() * 10 },
                onValueChangeFinished = {
                    Airwave.save(context)
                    Airwave.restartIfPlaying(context)
                },
                valueRange = BUFFER_MIN_MS.toFloat()..BUFFER_MAX_MS.toFloat(),
                modifier = Modifier.weight(1f),
                colors = SliderDefaults.colors(thumbColor = Accent, activeTrackColor = Accent, inactiveTrackColor = Border),
            )
            Text("${Airwave.bufferMs} ms", color = TextMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
        }
    }

    CardBox {
        CardTitle("Audio output", "How this phone plays the sound")
        OutputEngine.entries.forEach { engine ->
            val missing = (engine == OutputEngine.OpenSl && !NativeAudio.available) ||
                (engine == OutputEngine.Oboe && !NativeOboe.available)
            Choice(
                selected = Airwave.outputEngine == engine,
                title = engine.label,
                hint = if (missing) "Not in this build: install the Android NDK and rebuild. Uses AudioTrack meanwhile." else engine.hint,
            ) {
                Airwave.outputEngine = engine
                Airwave.save(context)
                Airwave.restartIfPlaying(context)
            }
        }
    }
}

@Composable
private fun Choice(selected: Boolean, title: String, hint: String, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(10.dp))
            .background(if (selected) AccentDark else BgMain)
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(
            Modifier
                .size(18.dp)
                .clip(CircleShape)
                .border(2.dp, if (selected) Accent else Faint, CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            if (selected) Box(Modifier.size(8.dp).clip(CircleShape).background(Accent))
        }
        Column(Modifier.weight(1f)) {
            Text(title, color = if (selected) TextMain else Muted, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            Text(hint, color = Faint, fontSize = 12.sp)
        }
    }
}

@Composable
private fun SendScreen() {
    val context = LocalContext.current
    val state = Airwave.sender
    var askedMic by remember { mutableStateOf(false) }
    val micPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { ok ->
        askedMic = true
        if (ok) Airwave.startSending(context)
    }
    val notifyPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { }

    Header("Send", "Share this phone's microphone with your computer.")
    CardBox {
        when (state) {
            SenderState.Off -> Status(Faint, "Not sending", "Press Start to share the microphone.")
            SenderState.Live -> Status(
                Accent,
                "Sending",
                "48 kHz · ${if (Airwave.stereo) "stereo" else "mono"} · ${Airwave.sendCodec.describe} · ${Airwave.listeners} listening",
            )
            is SenderState.Failed -> Status(Danger, "Could not send", state.message, Danger)
        }
        Column(
            Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(10.dp))
                .background(BgMain)
                .border(1.dp, Border, RoundedCornerShape(10.dp))
                .padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text("Your computer connects to", color = Muted, fontSize = 12.sp)
            Text(
                "${localAddress() ?: "no Wi-Fi"}:$STREAM_PORT",
                color = if (Airwave.isSending) Accent else TextMain,
                fontSize = 20.sp,
                fontWeight = FontWeight.Bold,
            )
            Meter(Airwave.senderLevel, Airwave.isSending)
        }
        if (Airwave.isSending) {
            QuietButton("Stop", Danger) { Airwave.stopSending(context) }
        } else {
            PrimaryButton("Start sending") {
                if (Build.VERSION.SDK_INT >= 33 &&
                    ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
                ) notifyPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
                if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
                    Airwave.startSending(context)
                } else {
                    micPermission.launch(Manifest.permission.RECORD_AUDIO)
                }
            }
        }
        if (askedMic && ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            Text("Microphone permission is needed to send.", color = Danger, fontSize = 12.sp)
        }
    }
    CardBox {
        CardTitle("Sound quality")
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Chip("Mono", !Airwave.stereo) { Airwave.stereo = false; Airwave.save(context) }
            Chip("Stereo", Airwave.stereo) { Airwave.stereo = true; Airwave.save(context) }
        }
        Text("Most phone microphones are mono. Applies next time you start.", color = Faint, fontSize = 12.sp)
    }
    CardBox {
        CardTitle("Compression", "Light halves the data, Strong uses a quarter. Helps on weak Wi-Fi")
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Codec.entries.forEach { c ->
                Chip(c.label, Airwave.sendCodec == c) { Airwave.sendCodec = c; Airwave.save(context) }
            }
        }
        Text("Applies next time you start.", color = Faint, fontSize = 12.sp)
    }
}

@Composable
private fun UsbScreen() {
    val context = LocalContext.current
    Header("USB", "Use a cable instead of Wi-Fi: steadier sound and less delay.")
    CardBox {
        CardTitle("Accessory mode", "No USB debugging needed")
        if (Airwave.accessoryAttached) {
            Status(Accent, "Connected by cable", "Sound from the computer plays here.")
        } else {
            Status(Faint, "Not connected", "")
        }
        Steps(
            "Plug this phone into the computer.",
            "On the computer, open Airwave → USB → Use accessory mode.",
            "Allow Airwave to open when the phone asks, then press Send sound on the computer.",
        )
    }
    CardBox {
        CardTitle("USB debugging (adb)", "Works with adb reverse and adb forward")
        Steps(
            "Turn on USB debugging on this phone and plug it in.",
            "On the computer, open Airwave → USB → Link.",
            "Computer to phone: press the button below. Phone to computer: start sending here, then press Listen to phone on the computer.",
        )
        PrimaryButton("Listen to computer over USB") {
            Airwave.listenTo(context, "127.0.0.1:$STREAM_PORT")
        }
    }
}

@Composable
private fun Steps(vararg steps: String) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        steps.forEachIndexed { i, s ->
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Box(
                    Modifier
                        .size(22.dp)
                        .clip(CircleShape)
                        .background(AccentDark),
                    contentAlignment = Alignment.Center,
                ) { Text("${i + 1}", color = Accent, fontSize = 12.sp, fontWeight = FontWeight.Bold) }
                Text(s, color = Muted, fontSize = 13.sp, modifier = Modifier.weight(1f))
            }
        }
    }
}

@Composable
private fun Header(title: String, sub: String) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(title, color = TextMain, fontSize = 26.sp, fontWeight = FontWeight.Bold)
        Text(sub, color = Muted, fontSize = 14.sp)
    }
    Spacer(Modifier.height(2.dp))
}
