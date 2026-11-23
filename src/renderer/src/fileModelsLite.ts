/**
 * The editor's open files, for code that must not pull in Monaco. Before any file was opened
 * Monaco isn't loaded and there are no models, so every call is a no-op.
 */
import type { editor } from 'monaco-editor'

interface FileModels {
  getModel(path: string): editor.ITextModel | null
  isDirty(model: editor.ITextModel): boolean
  disposeModel(path: string): void
  reloadIfClean(path: string): Promise<void>
  saveModel(path: string): Promise<void>
  languageFor(path: string): string
}

let fm: FileModels | null = null

export function registerFileModels(m: FileModels): void {
  fm = m
}

/** An open file with unsaved edits. */
export function hasUnsaved(path: string): boolean {
  const m = fm?.getModel(path)
  return !!m && fm!.isDirty(m)
}

export const disposeModel = (path: string): void => fm?.disposeModel(path)
/** Save an open file (no-op before Monaco loaded: nothing can be unsaved then). */
export const saveFile = async (path: string): Promise<void> => fm?.saveModel(path)
export const reloadIfClean = async (path: string): Promise<void> => fm?.reloadIfClean(path)
/** Monaco's language id for a file ('' until Monaco has loaded). */
export const languageOf = (path: string): string => fm?.languageFor(path) ?? ''
