// Wählt den passenden Editor: Videos → VideoEditor, sonst Bilder → ImageEditor.
import type { ComponentProps } from 'react';
import ImageEditor from './ImageEditor';
import { isVideo } from './thumbs';
import VideoEditor from './VideoEditor';

export default function Editor(props: ComponentProps<typeof ImageEditor>) {
  return isVideo(props.name) ? <VideoEditor {...props} /> : <ImageEditor {...props} />;
}
