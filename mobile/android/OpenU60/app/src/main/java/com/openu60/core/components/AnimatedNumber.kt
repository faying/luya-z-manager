package com.openu60.core.components

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.material3.LocalTextStyle
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.Dp
import kotlin.math.abs

// MARK: - DigitElement

private data class DigitElement(
    val id: Int,       // Stable column index (right-to-left)
    val value: String, // "0"-"9" or separator
    val isDigit: Boolean,
) {
    val digitValue: Int? get() = if (isDigit) value.toIntOrNull() else null
}

// MARK: - SingleDigitReel

private const val TOTAL_SETS = 7
private const val DIGITS_PER_SET = 10
private const val TOTAL_SLOTS = TOTAL_SETS * DIGITS_PER_SET
private const val MIDDLE_SET_START = (TOTAL_SETS / 2) * DIGITS_PER_SET

/** Shortest path on the mod-10 ring. Positive = forward, negative = backward. */
private fun shortestDelta(from: Int, to: Int): Int {
    val forward = (to - from + 10) % 10
    val backward = forward - 10
    return if (abs(forward) <= abs(backward)) forward else backward
}

@Composable
private fun SingleDigitReel(
    digit: Int,
    style: TextStyle,
    color: Color,
    isAnimated: Boolean,
    animationDurationMs: Int,
) {
    val tnum = remember(style) { style.copy(fontFeatureSettings = "tnum") }
    val measurer = rememberTextMeasurer()
    val density = LocalDensity.current
    val slotSizePx = remember(tnum) { measurer.measure("8", tnum).size }
    val slotHeightDp: Dp = with(density) { slotSizePx.height.toDp() }
    val slotWidthDp: Dp = with(density) { slotSizePx.width.toDp() }
    val slotHeightPx = slotSizePx.height.toFloat()

    var cumulativePosition by remember { mutableIntStateOf(MIDDLE_SET_START + digit) }
    val animatable = remember { androidx.compose.animation.core.Animatable(cumulativePosition.toFloat()) }

    LaunchedEffect(digit) {
        if (!isAnimated) {
            cumulativePosition = MIDDLE_SET_START + digit
            animatable.snapTo(cumulativePosition.toFloat())
            return@LaunchedEffect
        }
        val prevDigit = Math.floorMod(cumulativePosition, DIGITS_PER_SET)
        val delta = shortestDelta(prevDigit, digit)
        cumulativePosition += delta
        animatable.animateTo(
            cumulativePosition.toFloat(),
            animationSpec = androidx.compose.animation.core.tween(
                durationMillis = animationDurationMs,
                easing = androidx.compose.animation.core.EaseInOut,
            ),
        )
        val resetTarget = MIDDLE_SET_START + digit
        if (cumulativePosition != resetTarget) {
            cumulativePosition = resetTarget
            animatable.snapTo(resetTarget.toFloat())
        }
    }

    Box(
        modifier = Modifier
            .height(slotHeightDp)
            .width(slotWidthDp)
            .clipToBounds(),
    ) {
        Column(
            modifier = Modifier.graphicsLayer {
                translationY = -animatable.value * slotHeightPx
            },
        ) {
            for (i in 0 until TOTAL_SLOTS) {
                Text(
                    text = "${i % DIGITS_PER_SET}",
                    style = tnum,
                    color = color,
                    modifier = Modifier.height(slotHeightDp),
                )
            }
        }
    }
}

// MARK: - AnimatedNumber (Int overload)

@Composable
fun AnimatedNumber(
    value: Int,
    modifier: Modifier = Modifier,
    style: TextStyle = LocalTextStyle.current.copy(fontWeight = FontWeight.Bold),
    color: Color = Color.Unspecified,
    animationDurationMs: Int = 400,
    prefix: String? = null,
    suffix: String? = null,
    separator: String? = null,
) {
    val absValue = abs(value)
    val isNegative = value < 0
    val elements = remember(absValue, separator) { buildIntElements(absValue, separator) }

    AnimatedNumberRow(
        elements = elements,
        isNegative = isNegative,
        style = style,
        color = color,
        animationDurationMs = animationDurationMs,
        prefix = prefix,
        suffix = suffix,
        modifier = modifier,
    )
}

// MARK: - AnimatedNumber (Double overload)

@Composable
fun AnimatedNumber(
    value: Double,
    decimalPlaces: Int = 1,
    modifier: Modifier = Modifier,
    style: TextStyle = LocalTextStyle.current.copy(fontWeight = FontWeight.Bold),
    color: Color = Color.Unspecified,
    animationDurationMs: Int = 400,
    prefix: String? = null,
    suffix: String? = null,
) {
    val absValue = abs(value)
    val isNegative = value < 0
    val elements = remember(absValue, decimalPlaces) { buildDoubleElements(absValue, decimalPlaces) }

    AnimatedNumberRow(
        elements = elements,
        isNegative = isNegative,
        style = style,
        color = color,
        animationDurationMs = animationDurationMs,
        prefix = prefix,
        suffix = suffix,
        modifier = modifier,
    )
}

@Composable
private fun AnimatedNumberRow(
    elements: List<DigitElement>,
    isNegative: Boolean,
    style: TextStyle,
    color: Color,
    animationDurationMs: Int,
    prefix: String?,
    suffix: String?,
    modifier: Modifier = Modifier,
) {
    val resolvedColor = if (color != Color.Unspecified) color else LocalTextStyle.current.color
    var isAnimated by remember { mutableStateOf(false) }

    LaunchedEffect(Unit) { isAnimated = true }

    Row(modifier = modifier) {
        if (prefix != null) {
            Text(text = prefix, style = style, color = resolvedColor)
        }
        if (isNegative) {
            Text(text = "-", style = style, color = resolvedColor)
        }

        elements.forEach { element ->
            key(element.id) {
                if (element.isDigit) {
                    SingleDigitReel(
                        digit = element.digitValue ?: 0,
                        style = style,
                        color = resolvedColor,
                        isAnimated = isAnimated,
                        animationDurationMs = animationDurationMs,
                    )
                } else {
                    Text(text = element.value, style = style, color = resolvedColor)
                }
            }
        }

        if (suffix != null) {
            Text(text = suffix, style = style, color = resolvedColor)
        }
    }
}

// MARK: - Element building

private fun buildIntElements(absValue: Int, separator: String?): List<DigitElement> {
    val formatted = if (separator != null) {
        val str = java.text.NumberFormat.getIntegerInstance(java.util.Locale.US).format(absValue)
        str.replace(",", separator)
    } else {
        absValue.toString()
    }
    return formatted.reversed().mapIndexed { index, char ->
        DigitElement(id = index, value = char.toString(), isDigit = char.isDigit())
    }.reversed()
}

private fun buildDoubleElements(absValue: Double, decimalPlaces: Int): List<DigitElement> {
    val formatted = String.format("%.${decimalPlaces}f", absValue)
    return formatted.reversed().mapIndexed { index, char ->
        DigitElement(id = index, value = char.toString(), isDigit = char.isDigit())
    }.reversed()
}
