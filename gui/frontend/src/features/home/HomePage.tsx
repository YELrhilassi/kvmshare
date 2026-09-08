import type { Page } from "@/app/nav";
import RolePicker from "@/features/home/RolePicker";
import ShareStatus from "@/features/home/ShareStatus";
import ConnectInfo from "@/features/home/ConnectInfo";
import LiveOverview from "@/features/home/LiveOverview";
import QuickLinks from "@/features/home/QuickLinks";
import Updater from "@/features/home/Updater";

interface Props {
  onNavigate: (p: Page) => void;
}

// The dashboard: what this machine is doing right now (role, live state,
// who is connected, what is on the network) and the single start/stop
// control. Everything here is state; settings live on their own pages.
export default function HomePage({ onNavigate }: Props) {
  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-5xl px-10 py-14">
        <header className="mb-12 space-y-1">
          <h1 className="text-2xl font-semibold tracking-tight">This machine</h1>
          <p className="text-sm text-muted-foreground">
            What it is doing right now — and who is on the network.
          </p>
        </header>

        <div className="space-y-14">
          <RolePicker />

          <div className="grid gap-x-16 gap-y-12 lg:grid-cols-2">
            <ShareStatus />
            <ConnectInfo />
          </div>

          <LiveOverview />

          <QuickLinks onNavigate={onNavigate} />

          <Updater />
        </div>
      </div>
    </div>
  );
}