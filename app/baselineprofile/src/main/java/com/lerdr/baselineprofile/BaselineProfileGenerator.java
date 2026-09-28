package com.lerdr.baselineprofile;

import androidx.benchmark.macro.junit4.BaselineProfileRule;
import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.uiautomator.By;
import androidx.test.uiautomator.Direction;
import androidx.test.uiautomator.UiObject2;
import androidx.test.uiautomator.Until;
import org.junit.Rule;
import org.junit.Test;
import org.junit.runner.RunWith;

/**
 * Generates the app baseline profile committed at
 * {@code app/src/main/baseline-prof.txt}. The journey covers the dominant
 * startup cost: cold start → Compose inflation of the agents home →
 * LazyColumn scroll. Run on a rootable emulator via
 * {@code ./gradlew :baselineprofile:connectedDebugAndroidTest
 * -Pandroid.testInstrumentationRunnerArguments.class=com.lerdr.baselineprofile.BaselineProfileGenerator},
 * then copy the generated {@code baseline-prof.txt} out of
 * {@code baselineprofile/build/outputs/connected_android_test_additional_output/}.
 *
 * <p>Plain Java on purpose: {@code com.android.test} modules on AGP 9.4's
 * built-in Kotlin compile {@code .kt} fine but do not package
 * kotlin-stdlib into the test dex, so instrumentation dies with
 * ClassNotFoundException before the first test.
 */
@RunWith(AndroidJUnit4.class)
public class BaselineProfileGenerator {

    @Rule
    public BaselineProfileRule baselineProfileRule = new BaselineProfileRule();

    @Test
    public void generate() {
        baselineProfileRule.collect(
                "com.lerdr.app",
                scope -> {
                    scope.pressHome();
                    scope.startActivityAndWait();

                    UiObject2 scrollable =
                            scope.getDevice()
                                    .wait(Until.findObject(By.scrollable(true)), 5_000);
                    if (scrollable != null) {
                        scrollable.setGestureMargin(scope.getDevice().getDisplayWidth() / 5);
                        scrollable.fling(Direction.DOWN);
                        scope.getDevice().waitForIdle();
                        scrollable.fling(Direction.UP);
                        scope.getDevice().waitForIdle();
                    }
                    return null; // Function1 result is ignored
                });
    }
}
