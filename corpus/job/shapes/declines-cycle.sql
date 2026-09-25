-- declines: cycle
-- A triangle has no join tree, so there is no order to sweep in.
SELECT MIN(a.id) AS link FROM movie_link AS a, movie_link AS b, movie_link AS c
WHERE a.linked_movie_id = b.movie_id AND b.linked_movie_id = c.movie_id AND c.linked_movie_id = a.movie_id;
