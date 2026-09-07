import type { Page } from "@/app/nav";
import RolePicker from "@/features/home/RolePicker";
import ShareStatus from "@/features/home/ShareStatus";
import ConnectInfo from "@/features/home/ConnectInfo";
import QuickLinks from "@/features/home/QuickLinks";
import Updater from "@/features/home/Updater";

interface Props {
  onNavigate: (p: Page) => void;
}

// Dashboard: the role, the single start/stop control, and what this
// machine's address/connection looks like — laid out on a grid, with
// hairline sections only (no cards).
export default function HomePage({ onNavigate }: Props) {
  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-4xl px-10 py-14">
        <header className="mb-12 space-y-1">
          <h1 className="text-2xl font-semibold tracking-tight">This machine</h1>
          <p className="text-sm text-muted-foreground">
            Choose what it does, then start it from here.
          </p>
        </header>

        <div className="space-y-14">
          <RolePicker />

          <div className="grid gap-x-16 gap-y-12 lg:grid-cols-2">
            <ShareStatus />
            <ConnectInfo />
          </div>

          <QuickLinks onNavigate={onNavigate} />
          <Updater />
        </div>
      </div>
    </div>
  );
}