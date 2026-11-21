/*
 * SPDX-FileCopyrightText: The Desktop Subsystem for Android authors
 * SPDX-License-Identifier: Apache-2.0
 */

package io.github.thisisthepy.dsa.design;

import android.annotation.SuppressLint;

import androidx.preference.PreferenceCategory;
import androidx.preference.PreferenceGroupAdapter;
import androidx.recyclerview.widget.RecyclerView;

/**
 * Groups a settings list into cards, one per category: the category's title stays above its
 * card, and the preferences under it are the card's rows. Only apps whose settings are an
 * androidx preference list get this file (apps.json says which).
 */
public final class PreferenceCards {
    private PreferenceCards() {
    }

    /** Adds the cards to `list`, the RecyclerView of a PreferenceFragmentCompat. */
    // PreferenceGroupAdapter is the adapter every PreferenceFragmentCompat list has; it is
    // public but marked for the library's own use. Only its item lookup is used.
    @SuppressLint("RestrictedApi")
    public static void attach(RecyclerView list) {
        CardGroups.attach(list, position -> {
            final RecyclerView.Adapter<?> adapter = list.getAdapter();
            return adapter instanceof PreferenceGroupAdapter
                    && ((PreferenceGroupAdapter) adapter).getItem(position) instanceof PreferenceCategory;
        });
    }
}
