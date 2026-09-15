package com.openu60.core.model

data class BandConfig(
    val nrBands: List<Int> = emptyList(),
    val lteBands: List<Int> = emptyList(),
    val locked: Boolean = false,
) {
    companion object {
        val COMMON_NR_BANDS = listOf(1, 2, 3, 5, 7, 8, 12, 20, 25, 28, 38, 40, 41, 48, 66, 71, 77, 78, 79)
        val COMMON_LTE_BANDS = listOf(1, 2, 3, 4, 5, 7, 8, 12, 13, 14, 17, 20, 25, 26, 28, 29, 30, 38, 40, 41, 48, 66, 71)
    }
}
