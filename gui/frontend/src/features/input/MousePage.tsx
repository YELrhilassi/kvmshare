import { useCallback, useEffect, useState } from "react";
import { api, type LayoutConfig, type InputSection } from "@/lib/bridge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Slider } from "@/components/ui/slider";
import { PageSkeleton } from "@/components/PageSkeleton";
import { cn } from "@/lib/utils";

// Mouse — the pointer half of the old Input & shortcuts page. The
// transforms here apply to the shared stream on its way to whichever
// machine is being controlled: they change how far the cursor travels
// and how the wheel scrolls, never what the buttons mean (buttons are
// forwarded by physical identity, like keys).
//
// Every control previews its effect: the speed strip shows the travel
// multiplier as a filled band with the 1× mark, and the wheel preview
// ticks a real page as notches arrive from the sliders below.

interface PointerPreviewProps {
  speed: number;
}

/** Travel band: how far one hand movement moves the remote cursor. */
function PointerPreview({ speed }: PointerPreviewProps) {
  // The band's fill: 1× is exactly half the strip; ±3× spans it all.
  const fill = Math.min(1, Math.max(0, 0.5 * (1 + Math.log2(speed) / 2)));
  return (
    <div className="mt-3">
      <div className="relative h-2 overflow-hidden rounded-full bg-muted">
        <div
          className="absolute inset-y-0 left-0 rounded-full bg-primary/70 transition-[width] duration-150"
          style={{ width: `${fill * 100}%` }}
        />
        {/* the 1× mark: "mirrors this machine" */}
        <div className="absolute inset-y-0 left-1/2 w-px bg-foreground/40" />
      </div>
      <div className="mt-1 flex justify-between text-[10px] text-muted-foreground/70">
        <span>slow</span>
        <span>1× mirrors this machine</span>
        <span>fast</span>
      </div>
    </div>
  );
}

interface WheelPreviewProps {
  notches: number;
}

/** A miniature page that scrolls as wheel notches land. */
function WheelPreview({ notches }: WheelPreviewProps) {
  const lines = 9;
  const max = Math.max(0, lines - 3);
  const offset = Math.min(max, Math.max(-max, notches));
  return (
    <div className="mt-3 h-[72px] overflow-hidden rounded-lg border border-border/60 bg-muted/20 p-1.5">
      <div
        className="space-y-1 transition-transform duration-150"
        style={{ transform: `translateY(${offset * -8}px)` }}
      >
        {Array.from({ length: lines + 6 }, (_, i) => (
          <div
            key={i}
            className="h-1.5 rounded-full bg-muted-foreground/25"
            style={{ width: `${55 + ((i * 37) % 45)}%` }}
          />
        ))}
      </div>
    </div>
  );
}

export default function MousePage() {
  const [config, setConfig] = useState<LayoutConfig | null>(null);
  const [loadErr, setLoadErr] = useState("");
  const [err, setErr] = useState("");
  const [saved, setSaved] = useState(false);
  // Wheel preview state: accumulated virtual notches, eased back to rest.
  const [notches, setNotches] = useState(0);

  const load = useCallback(async () => {
    setLoadErr("");
    try {
      setConfig(await api().LoadConfig());
    } catch (e) {
      setLoadErr(String(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const save = useCallback(
    async (patch: Partial<Pick<LayoutConfig, "input">>) => {
      if (!config) return;
      setErr("");
      try {
        const next = { ...config, ...patch };
        await api().SaveConfig(next);
        setConfig(next);
        setSaved(true);
        window.setTimeout(() => setSaved(false), 1500);
      } catch (e) {
        setErr(String(e));
      }
    },
    [config],
  );

  const input: InputSection = config?.input ?? { pointerSpeed: 1, wheelSpeed: 1, swapScroll: false };

  // A wheel-speed change ticks the preview once by the delta, so the
  // operator *feels* the multiplier instead of reading a number.
  useEffect(() => {
    if (!config) return;
    setNotches((n) => Math.max(-6, Math.min(6, n + Math.sign(input.wheelSpeed - 1) * Math.ceil(Math.abs(input.wheelSpeed - 1) * 2))));
  }, [config, input.wheelSpeed]);

  if (loadErr) {
    return (
      <div className="h-full overflow-y-auto">
        <div className="mx-auto w-full max-w-3xl px-8 py-8">
          <h1 className="text-lg font-semibold tracking-tight">Mouse</h1>
          <p className="mt-4 text-sm text-destructive">Could not load: {loadErr}</p>
          <Button className="mt-3" variant="outline" onClick={() => void load()}>
            Retry
          </Button>
        </div>
      </div>
    );
  }
  if (!config) return <PageSkeleton rows={3} />;

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto w-full max-w-3xl px-8 py-8">
        <h1 className="text-lg font-semibold tracking-tight">Mouse</h1>
        <p className="mt-1 max-w-2xl text-sm text-muted-foreground">
          How the shared pointer behaves on whichever machine it controls. Buttons are forwarded by physical identity —
          these settings change travel and scroll, never what a click means.
        </p>

        <div className="mt-8 space-y-6">
          <section className="rounded-xl border border-border/60 bg-card/40 p-5">
            <div className="flex items-baseline justify-between">
              <h2 className="text-sm font-medium">Pointer speed</h2>
              <span className="font-mono text-xs text-muted-foreground">{input.pointerSpeed.toFixed(2)}×</span>
            </div>
            <p className="mt-0.5 text-xs text-muted-foreground">
              How far the cursor travels on the controlled machine for the same hand movement.
            </p>
            <Slider
              className="mt-4"
              min={0.25}
              max={3}
              step={0.05}
              value={[input.pointerSpeed]}
              onValueChange={([v]) => void save({ input: { ...input, pointerSpeed: v } })}
            />
            <PointerPreview speed={input.pointerSpeed} />
          </section>

          <section className="rounded-xl border border-border/60 bg-card/40 p-5">
            <div className="flex items-baseline justify-between">
              <h2 className="text-sm font-medium">Wheel speed</h2>
              <span className="font-mono text-xs text-muted-foreground">{input.wheelSpeed.toFixed(2)}×</span>
            </div>
            <p className="mt-0.5 text-xs text-muted-foreground">Scroll distance per wheel notch on the controlled machine.</p>
            <Slider
              className="mt-4"
              min={0.25}
              max={8}
              step={0.25}
              value={[input.wheelSpeed]}
              onValueChange={([v]) => void save({ input: { ...input, wheelSpeed: v } })}
            />
            <WheelPreview notches={notches} />
          </section>

          <section className="rounded-xl border border-border/60 bg-card/40 p-5">
            <div className="flex items-center justify-between gap-6">
              <div>
                <h2 className="text-sm font-medium">Natural scrolling</h2>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  Invert the vertical wheel direction, like a touchscreen (content follows the fingers).
                </p>
              </div>
              <Switch checked={input.swapScroll} onCheckedChange={(v) => void save({ input: { ...input, swapScroll: v } })} />
            </div>
          </section>
        </div>

        {(err || saved) && (
          <p className={cn("mt-4 text-xs", err ? "text-destructive" : "text-emerald-600")}>{err || "Saved"}</p>
        )}
      </div>
    </div>
  );
}
