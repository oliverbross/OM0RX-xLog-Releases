import type { Instrument } from "./elecraftClient";

const scales = {
  kpa: [
    { label: "Output", low: 0, high: 500, unit: "W", color: "#51e3c1" },
    { label: "SWR", low: 1, high: 5, unit: ":1", color: "#ffbb70" },
    { label: "Temperature", low: 0, high: 100, unit: "°C", color: "#89c6ff" },
  ],
  kat: [
    { label: "Last SWR", low: 1, high: 5, unit: ":1", color: "#87aaff" },
    { label: "Last bypass SWR", low: 1, high: 5, unit: ":1", color: "#eda6ff" },
  ],
};

export function InstrumentGauges({
  device,
  items,
}: {
  device: Instrument;
  items: [string, string][];
}) {
  const values = new Map(items);
  return (
    <div className="ec-gauges">
      {scales[device].map(({ label, low, high, unit, color }) => {
        const reading = values.get(label) ?? "—";
        const number = parseFloat(reading);
        const fraction = Math.max(
          0,
          Math.min(1, (number - low) / (high - low)),
        );
        const display = Number.isFinite(number) ? String(number) : reading;
        return (
          <figure
            key={label}
            className="ec-gauge"
            aria-label={label + " " + reading}
            style={{ color }}
          >
            <svg viewBox="0 0 128 84" aria-hidden="true">
              <path d="M12 64 A52 52 0 0 1 116 64" className="ec-track" />
              {Number.isFinite(number) && (
                <path
                  d="M12 64 A52 52 0 0 1 116 64"
                  className="ec-arc"
                  pathLength="100"
                  strokeDasharray={fraction * 100 + " 100"}
                />
              )}
              {Array.from({ length: 11 }, (_, i) => (
                <line
                  key={i}
                  x1="22"
                  x2={i % 5 === 0 ? "28" : "25"}
                  y1="64"
                  y2="64"
                  transform={"rotate(" + i * 18 + " 64 64)"}
                  className="ec-tick"
                />
              ))}
              <text x="64" y="58" textAnchor="middle" className="ec-reading">
                {display}
              </text>
              {Number.isFinite(number) && (
                <text x="64" y="73" textAnchor="middle" className="ec-unit">
                  {unit}
                </text>
              )}
              <text x="12" y="80" textAnchor="middle" className="ec-limit">
                {low}
              </text>
              <text x="116" y="80" textAnchor="middle" className="ec-limit">
                {high}
              </text>
            </svg>
            <figcaption>{label}</figcaption>
          </figure>
        );
      })}
    </div>
  );
}
