import CompanionAudioPanel from "./CompanionAudioPanel";

export default function CompanionBridgePanel() {
  return <section aria-labelledby="companion-bridge-title">
    <h3 id="companion-bridge-title">OM0RX-xNexus</h3>
    <p>Choose your audio connections once. xLog remembers them and connects to xNexus automatically.</p>
    <CompanionAudioPanel />
  </section>;
}
