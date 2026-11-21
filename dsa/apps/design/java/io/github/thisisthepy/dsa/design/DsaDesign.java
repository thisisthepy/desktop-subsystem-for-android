/*
 * SPDX-FileCopyrightText: The Desktop Subsystem for Android authors
 * SPDX-License-Identifier: Apache-2.0
 */

package io.github.thisisthepy.dsa.design;

import android.content.Context;

import com.google.android.material.appbar.AppBarLayout;

/**
 * The rules every default app shares for its large title area.
 *
 * <p>A large title is made for a phone held in one hand. A desktop window is often short, and
 * an expanded title there takes space the list needs, so a window lower than
 * {@link #LARGE_TITLE_MIN_HEIGHT_DP} opens with the title already folded into the toolbar.
 * Changing the window's height recreates the activity (the height is part of its
 * configuration), so the rule is applied again at the new height.
 */
public final class DsaDesign {
    /** The window height, in dp, from which a screen opens with its large title expanded. */
    public static final int LARGE_TITLE_MIN_HEIGHT_DP = 600;

    private DsaDesign() {
    }

    /** Whether a screen in this window opens with its large title expanded. */
    public static boolean largeTitleStartsExpanded(Context context) {
        return largeTitleStartsExpanded(context.getResources().getConfiguration().screenHeightDp);
    }

    /** Whether a window `heightDp` high opens with its large title expanded. */
    public static boolean largeTitleStartsExpanded(int heightDp) {
        return heightDp >= LARGE_TITLE_MIN_HEIGHT_DP;
    }

    /** Expands or folds `appBar` at once, as a screen opening in its window should be. */
    public static void showLargeTitle(AppBarLayout appBar) {
        appBar.setExpanded(largeTitleStartsExpanded(appBar.getContext()), false);
    }
}
