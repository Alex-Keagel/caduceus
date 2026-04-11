import { useState, useCallback } from "react";

const ONBOARDING_KEY = "caduceus_onboarding_complete";

export function useOnboarding() {
  const [tourComplete, setTourComplete] = useState(
    () => localStorage.getItem(ONBOARDING_KEY) === "true"
  );
  const [helpOpen, setHelpOpen] = useState(false);
  const [helpOverlayOpen, setHelpOverlayOpen] = useState(false);

  const completeTour = useCallback(() => {
    localStorage.setItem(ONBOARDING_KEY, "true");
    setTourComplete(true);
  }, []);

  const resetTour = useCallback(() => {
    localStorage.removeItem(ONBOARDING_KEY);
    setTourComplete(false);
  }, []);

  const toggleHelp = useCallback(() => setHelpOpen((p) => !p), []);
  const toggleHelpOverlay = useCallback(() => setHelpOverlayOpen((p) => !p), []);

  return {
    tourComplete,
    completeTour,
    resetTour,
    helpOpen,
    toggleHelp,
    setHelpOpen,
    helpOverlayOpen,
    toggleHelpOverlay,
    setHelpOverlayOpen,
  };
}
