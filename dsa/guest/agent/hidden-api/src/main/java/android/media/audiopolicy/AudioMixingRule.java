package android.media.audiopolicy;

/**
 * Which sound an audio mix takes; declarations only. The rule kinds (RULE_MATCH_UID) and the
 * roles (MIX_ROLE_PLAYERS) are read from the framework's class at run time, so no value is
 * copied here.
 */
public class AudioMixingRule {
    private AudioMixingRule() {
        throw new RuntimeException("Stub!");
    }

    /** Makes a rule. */
    public static class Builder {
        public Builder() {
            throw new RuntimeException("Stub!");
        }

        public Builder setTargetMixRole(int mixRole) {
            throw new RuntimeException("Stub!");
        }

        public Builder addMixRule(int rule, Object property) {
            throw new RuntimeException("Stub!");
        }

        public AudioMixingRule build() {
            throw new RuntimeException("Stub!");
        }
    }
}
